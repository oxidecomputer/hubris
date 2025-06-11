// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SPD control task for Cosmo

#![no_std]
#![no_main]

use drv_cpu_seq_api::PowerState;
use idol_runtime::RequestError;
use ringbuf::{ringbuf, ringbuf_entry};
use task_jefe_api::Jefe;
use userlib::{
    sys_get_timer, sys_recv_notification, sys_set_timer, task_slot,
    FromPrimitive, RecvMessage,
};

task_slot!(JEFE, jefe);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Ready,
}

ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    // Wait for entry to A2 before we enable our i2c controller.
    let jefe = Jefe::from(JEFE.get_task_id());
    loop {
        // This laborious list is intended to ensure that new power states
        // have to be added explicitly here.
        match PowerState::from_u32(jefe.get_state()) {
            Some(PowerState::A2)
            | Some(PowerState::A2PlusFans)
            | Some(PowerState::A1)
            | Some(PowerState::A0)
            | Some(PowerState::A0PlusHP)
            | Some(PowerState::A0Reset)
            | Some(PowerState::A0Thermtrip) => {
                break;
            }
            None => {
                // This happens before we're in a valid power state.
                //
                // Only listen to our Jefe notification.
                sys_recv_notification(notifications::JEFE_STATE_CHANGE_MASK);
            }
        }
    }

    ringbuf_entry!(Trace::Ready);

    let mut server = ServerImpl { deadline: 0u64 };
    sys_set_timer(Some(0), notifications::TIMER_MASK);
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

// Poll the thermal sensors at roughly 4 Hz
const TIMER_INTERVAL: u64 = 250;

struct ServerImpl {
    deadline: u64,
}

impl idl::InOrderCosmoSpdImpl for ServerImpl {
    fn ping(
        &mut self,
        _mgs: &RecvMessage,
    ) -> Result<u8, RequestError<core::convert::Infallible>> {
        Ok(0)
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        let now = sys_get_timer().now;
        if now >= self.deadline {
            self.deadline = now + TIMER_INTERVAL;
        }
        sys_set_timer(Some(self.deadline), notifications::TIMER_MASK);
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_periph.rs"));
}

mod idl {
    use super::*;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
