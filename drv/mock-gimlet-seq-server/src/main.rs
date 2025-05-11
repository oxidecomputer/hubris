// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Gimlet sequencing process.

#![no_std]
#![no_main]

use drv_cpu_seq_api::{PowerState, SeqError, StateChangeReason, Transition};
use idol_runtime::{NotificationHandler, RequestError};
use task_jefe_api::Jefe;
use userlib::{FromPrimitive, RecvMessage, UnwrapLite};

userlib::task_slot!(JEFE, jefe);

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl::init(Jefe::from(JEFE.get_task_id()));

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    jefe: Jefe,
}

impl ServerImpl {
    fn init(jefe: Jefe) -> Self {
        let me = Self { jefe };
        me.set_state_impl(PowerState::A2);
        me
    }

    fn get_state_impl(&self) -> PowerState {
        // Only we should be setting the state, and we set it to A2 on startup;
        // this conversion should never fail.
        PowerState::from_u32(self.jefe.get_state()).unwrap_lite()
    }

    fn set_state_impl(&self, state: PowerState) {
        self.jefe.set_state(state as u32);
    }

    fn validate_state_change(
        &self,
        state: PowerState,
    ) -> Result<Transition, SeqError> {
        match (self.get_state_impl(), state) {
            (PowerState::A2, PowerState::A0)
            | (PowerState::A0, PowerState::A2)
            | (PowerState::A0PlusHP, PowerState::A2)
            | (PowerState::A0Thermtrip, PowerState::A2) => Ok(Transition::Done),

            (current, next) if current == next => Ok(Transition::NoChange),

            _ => Err(SeqError::IllegalTransition),
        }
    }
}

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
        let transition = self.validate_state_change(state)?;
        if transition == Transition::Done {
            self.set_state_impl(state);
        }
        Ok(transition)
    }

    fn set_state_with_reason(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
        _: StateChangeReason,
    ) -> Result<Transition, RequestError<SeqError>> {
        let transition = self.validate_state_change(state)?;
        if transition == Transition::Done {
            self.set_state_impl(state);
        }
        Ok(transition)
    }

    fn send_hardware_nmi(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
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
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

mod idl {
    use super::StateChangeReason;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
