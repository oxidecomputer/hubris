// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Front IO board

#![no_std]
#![no_main]

use core::convert::Infallible;
use drv_front_io_api::FrontIOError;
use drv_sidecar_seq_api::Sequencer;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use userlib::*;

task_slot!(SEQUENCER, sequencer);

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
}
ringbuf!(Trace, 32, Trace::None);

struct ServerImpl {
    seq: Sequencer,
}

impl idl::InOrderFrontIOImpl for ServerImpl {
    /// Returns true if a front IO board was determined to be present and powered on
    fn board_present(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.seq.front_io_board_present())
    }

    /// Returns if the front IO board has completely sequenced and is ready
    fn board_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<FrontIOError>> {
        self.seq
            .front_io_board_ready()
            .map_err(|_| FrontIOError::SeqError)
            .map_err(RequestError::from)
    }

    fn phy_reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.seq
            .reset_front_io_phy()
            .map_err(|_| FrontIOError::SeqError)
            .map_err(RequestError::from)
    }

    /// Set the internal state of the PHY's oscillator
    fn phy_set_osc_state(
        &mut self,
        _: &RecvMessage,
        good: bool,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.seq
            .set_front_io_phy_osc_state(good)
            .map_err(|_| FrontIOError::SeqError)
            .map_err(RequestError::from)
    }
}

// notifications are not supported at this time
impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: NotificationBits) {
        unreachable!()
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let mut server = ServerImpl {
        seq: Sequencer::from(SEQUENCER.get_task_id()),
    };

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::FrontIOError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
