// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Grapefruit FPGA process.

#![no_std]
#![no_main]

use drv_cpu_seq_api::{PowerState, SeqError, StateChangeReason, Transition};
use drv_spartan7_loader_api::Spartan7Loader;
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};
use task_jefe_api::Jefe;
use task_packrat_api::{
    CacheSetError, MacAddressBlock, OxideIdentity, Packrat,
};
use userlib::{hl, task_slot, FromPrimitive, RecvMessage, UnwrapLite};

use ringbuf::{counted_ringbuf, ringbuf_entry, Count};

task_slot!(JEFE, jefe);
task_slot!(LOADER, spartan7_loader);

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    #[count(skip)]
    None,

    MacsAlreadySet(MacAddressBlock),
    IdentityAlreadySet(OxideIdentity),
}

counted_ringbuf!(Trace, 128, Trace::None);

task_slot!(SYS, sys);
task_slot!(PACKRAT, packrat);

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    // Populate packrat with dummy values, because talking to the EEPROM is hard
    let packrat = Packrat::from(PACKRAT.get_task_id());
    let macs = MacAddressBlock {
        base_mac: [0; 6],
        count: 0.into(),
        stride: 0,
    };
    match packrat.set_mac_address_block(macs) {
        Ok(()) => (),
        Err(CacheSetError::ValueAlreadySet) => {
            ringbuf_entry!(Trace::MacsAlreadySet(macs));
        }
    }
    let identity = OxideIdentity {
        serial: *b"GRAPEFRUIT\0",
        part_number: *b"913-0000083",
        revision: 0,
    };
    match packrat.set_identity(identity) {
        Ok(()) => (),
        Err(CacheSetError::ValueAlreadySet) => {
            ringbuf_entry!(Trace::IdentityAlreadySet(identity));
        }
    }

    let mut server = ServerImpl::init(&sys);
    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

#[allow(unused)]
struct ServerImpl {
    jefe: Jefe,
    sgpio: fmc_periph::Sgpio,
}

impl ServerImpl {
    fn init(sys: &sys_api::Sys) -> Self {
        // Ensure the SP fault pin is configured as an open-drain output, and
        // pull it low to make the sequencer restart externally visible.
        const FAULT_PIN_L: sys_api::PinSet = sys_api::Port::A.pin(15);
        sys.gpio_configure_output(
            FAULT_PIN_L,
            sys_api::OutputType::OpenDrain,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );
        sys.gpio_reset(FAULT_PIN_L);

        // Wait for the FPGA to be loaded
        let loader = Spartan7Loader::from(LOADER.get_task_id());

        let server = Self {
            jefe: Jefe::from(JEFE.get_task_id()),
            sgpio: fmc_periph::Sgpio::new(loader.get_token()),
        };

        // Note that we don't use `Self::set_state_impl` here, as that will
        // first attempt to get the current power state from `jefe`, and we
        // haven't set it yet!
        server.jefe.set_state(PowerState::A2 as u32);

        // Clear the external fault now that we're about to start serving
        // messages and fewer things can go wrong.
        sys.gpio_set(FAULT_PIN_L);

        server
    }

    fn get_state_impl(&self) -> PowerState {
        // Only we should be setting the state, and we set it to A2 on startup;
        // this conversion should never fail.
        PowerState::from_u32(self.jefe.get_state()).unwrap_lite()
    }

    fn set_state_impl(
        &self,
        state: PowerState,
    ) -> Result<Transition, SeqError> {
        match (self.get_state_impl(), state) {
            (PowerState::A2, PowerState::A0)
            | (PowerState::A0, PowerState::A2)
            | (PowerState::A0PlusHP, PowerState::A2)
            | (PowerState::A0Thermtrip, PowerState::A2) => {
                self.jefe.set_state(state as u32);
                Ok(Transition::Changed)
            }

            (current, requested) if current == requested => {
                Ok(Transition::Unchanged)
            }

            _ => Err(SeqError::IllegalTransition),
        }
    }
}

// The `Sequencer` implementation for Grapefruit is copied from
// `mock-gimlet-seq-server`.  State is set to Jefe, but isn't actually
// controlled here.
impl idl::InOrderSequencerImpl for ServerImpl {
    fn get_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<PowerState, RequestError<core::convert::Infallible>> {
        Ok(self.get_state_impl())
    }

    fn set_state(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
    ) -> Result<Transition, RequestError<SeqError>> {
        Ok(self.set_state_impl(state)?)
    }

    fn set_state_with_reason(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
        _: StateChangeReason,
    ) -> Result<Transition, RequestError<SeqError>> {
        Ok(self.set_state_impl(state)?)
    }

    fn send_hardware_nmi(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        // p5 is MGMT_ASSERT_NMI_BTN_L
        self.sgpio.out1.set_p5(false);
        hl::sleep_for(1000);
        self.sgpio.out1.set_p5(true);
        Ok(())
    }

    fn read_fpga_regs(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 64], RequestError<core::convert::Infallible>> {
        Ok([0; 64])
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

mod idl {
    use drv_cpu_seq_api::StateChangeReason;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_sgpio.rs"));
}
