// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Jeffrey --- Jefe's little helper

#![no_std]
#![no_main]

task_slot!(JEFE, jefe);

#[export_name = "main"]
fn main() -> ! {}

////////////////////////////////////////////////////////////////////////////////

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
