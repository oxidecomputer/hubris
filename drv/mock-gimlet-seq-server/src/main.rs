// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Gimlet sequencing process.

#![no_std]
#![no_main]

use drv_gimlet_seq_api::{PowerState, SeqError};
use idol_runtime::RequestError;
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
    lease_expiration: Option<u64>,
}

impl ServerImpl {
    fn init(jefe: Jefe) -> Self {
        let me = Self {
            jefe,
            lease_expiration: None,
        };
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
}

impl idl::InOrderSequencerImpl for ServerImpl {
    fn get_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<PowerState, RequestError<SeqError>> {
        Ok(self.get_state_impl())
    }

    fn set_state(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
    ) -> Result<(), RequestError<SeqError>> {
        match (self.get_state_impl(), state) {
            (PowerState::A2, PowerState::A0)
            | (PowerState::A0, PowerState::A2)
            | (PowerState::A0PlusHP, PowerState::A2)
            | (PowerState::A0Thermtrip, PowerState::A2) => {
                self.set_state_impl(state);
                Ok(())
            }

            _ => Err(RequestError::Runtime(SeqError::IllegalTransition)),
        }
    }

    fn fans_on(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SeqError>> {
        Ok(())
    }

    fn fans_off(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SeqError>> {
        Ok(())
    }

    fn send_hardware_nmi(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        Ok(())
    }

    fn lease_devices(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u64, RequestError<SeqError>> {
        const LEASE_LENGTH: u64 = 100;
        let expiration = userlib::sys_get_timer().now + LEASE_LENGTH;
        self.lease_expiration = Some(expiration);
        Ok(expiration)
    }
}

mod idl {
    use super::{PowerState, SeqError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
