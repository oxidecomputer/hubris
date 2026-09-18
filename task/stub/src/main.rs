// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A server that agrees with everyone.
//!
//! Every message it receives is answered with response code 0 and an empty
//! reply, and leases are left untouched. It stands in for a driver that a
//! simulation does not care about, such as the LED driver in a host run of
//! ping and pong, so that its clients see success without any hardware
//! behind it.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use userlib::{sys_recv_open, sys_reply};

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    let mut message = [0u8; 256];
    loop {
        let received = sys_recv_open(&mut message, 0);
        sys_reply(received.sender, 0, &[]);
    }
}
