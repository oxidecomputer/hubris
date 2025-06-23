// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SPD control task for Cosmo

#![no_std]
#![no_main]

use drv_cpu_seq_api::PowerState;
use drv_spartan7_loader_api::Spartan7Loader;
use idol_runtime::RequestError;
use ringbuf::{ringbuf, ringbuf_entry};
use task_jefe_api::Jefe;
use task_packrat_api::Packrat;
use userlib::{
    hl::sleep_for, sys_recv_notification, sys_set_timer, task_slot,
    FromPrimitive, RecvMessage,
};
use zerocopy::IntoBytes;

task_slot!(JEFE, jefe);
task_slot!(PACKRAT, packrat);
task_slot!(LOADER, spartan7_loader);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Ready,
    Present { index: usize, present: bool },
}

ringbuf!(Trace, 32, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    // Wait for entry to A0 before we enable our i2c controller.  Normally,
    // we'd be able to read SPDs in A2, but there's a hardware errata:
    // https://github.com/oxidecomputer/hardware-cosmo/issues/689
    let jefe = Jefe::from(JEFE.get_task_id());
    loop {
        // This laborious list is intended to ensure that new power states
        // have to be added explicitly here.
        match PowerState::from_u32(jefe.get_state()) {
            Some(PowerState::A0) | Some(PowerState::A0PlusHP) => {
                break;
            }
            Some(PowerState::A2)
            | Some(PowerState::A2PlusFans)
            | Some(PowerState::A1)
            | Some(PowerState::A0Reset)
            | Some(PowerState::A0Thermtrip)
            | None => {
                // This happens before we're in a valid power state.
                //
                // Only listen to our Jefe notification.
                sys_recv_notification(notifications::JEFE_STATE_CHANGE_MASK);
            }
        }
    }

    ringbuf_entry!(Trace::Ready);

    // Time to get the SPD data from the FPGA!
    let packrat = Packrat::from(PACKRAT.get_task_id());
    let loader = Spartan7Loader::from(LOADER.get_task_id());
    let token = loader.get_token();
    let dimms = fmc_periph::Dimms::new(token);

    // Kick off a read then wait for it to complete
    dimms.spd_ctrl.modify(|s| s.set_start(true));
    while dimms.spd_ctrl.start() {
        sleep_for(10);
    }

    let mut present = [false; DIMM_COUNT];
    for (index, present) in present.iter_mut().enumerate() {
        // Check if this channel is present
        *present = match index {
            0 => dimms.spd_present.bus0_a(),
            1 => dimms.spd_present.bus0_b(),
            2 => dimms.spd_present.bus0_c(),
            3 => dimms.spd_present.bus0_d(),
            4 => dimms.spd_present.bus0_e(),
            5 => dimms.spd_present.bus0_f(),
            6 => dimms.spd_present.bus1_g(),
            7 => dimms.spd_present.bus1_h(),
            8 => dimms.spd_present.bus1_i(),
            9 => dimms.spd_present.bus1_j(),
            10 => dimms.spd_present.bus1_k(),
            11 => dimms.spd_present.bus1_l(),
            _ => unreachable!(),
        };
        ringbuf_entry!(Trace::Present {
            index,
            present: *present
        });
        if !*present {
            continue;
        }
        // Set this channel as selected, clearing other selections
        dimms.spd_select.modify(|s| {
            s.set_bus0_a(false);
            s.set_bus0_b(false);
            s.set_bus0_c(false);
            s.set_bus0_d(false);
            s.set_bus0_e(false);
            s.set_bus0_f(false);
            s.set_bus1_g(false);
            s.set_bus1_h(false);
            s.set_bus1_i(false);
            s.set_bus1_j(false);
            s.set_bus1_k(false);
            s.set_bus1_l(false);
            match index {
                0 => s.set_bus0_a(true),
                1 => s.set_bus0_b(true),
                2 => s.set_bus0_c(true),
                3 => s.set_bus0_d(true),
                4 => s.set_bus0_e(true),
                5 => s.set_bus0_f(true),
                6 => s.set_bus1_g(true),
                7 => s.set_bus1_h(true),
                8 => s.set_bus1_i(true),
                9 => s.set_bus1_j(true),
                10 => s.set_bus1_k(true),
                11 => s.set_bus1_l(true),
                _ => unreachable!(),
            }
        });

        // Read 4x256 bytes from the FPGA's buffer and copy to Packrat
        dimms.spd_rd_ptr.set_addr(0);
        for i in 0..4 {
            // Limited by max lease size for Packrat
            let mut buf = [0u32; 64];
            for b in &mut buf {
                *b = dimms.spd_rdata.data();
            }
            packrat.set_spd_eeprom(index as u8, i * 256, buf.as_bytes());
        }
    }

    let mut server = ServerImpl {
        deadline: 0u64,
        dimms,
        present,
    };
    sys_set_timer(Some(0), notifications::TIMER_MASK);
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

const DIMM_COUNT: usize = 12;

#[allow(unused)]
struct ServerImpl {
    deadline: u64,
    dimms: fmc_periph::Dimms,
    present: [bool; DIMM_COUNT],
}

impl idl::InOrderCosmoSpdImpl for ServerImpl {
    fn ping(
        &mut self,
        _mgs: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        Ok(())
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        // Nothing to do here
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_periph.rs"));
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
