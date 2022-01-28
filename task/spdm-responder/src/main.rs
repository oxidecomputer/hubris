// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

// NOTE: you will probably want to remove this when you write your actual code;
// we need to import userlib to get this to compile, but it throws a warning
// because we're not actually using it yet!
#[allow(unused_imports)]
use userlib::*;

use ringbuf::{ringbuf, ringbuf_entry};
use spdm::{
    config::NUM_SLOTS,
    crypto::{FakeSigner, FilledSlot},
};

/// Record the types and sizes of the messages sent and received by this server
#[derive(Copy, Clone, PartialEq, Debug)]
enum LogMsg {
    // Static initializer
    Init,
    Received { code: u8, size: u16 },
    Sent { code: u8, size: u16 },
    State(&'static str),
}

#[export_name = "main"]
fn main() -> ! {
    ringbuf!(LogMsg, 16, LogMsg::Init);
    const EMPTY_SLOT: Option<FilledSlot<'_, FakeSigner>> = None;
    let slots = [EMPTY_SLOT; NUM_SLOTS];
    let mut responder = spdm::Responder::new(slots);
    ringbuf_entry!(LogMsg::State(responder.state().name()));
    loop {}
}
