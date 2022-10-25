// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_sp_ctrl_api::*;
use ringbuf::*;
//use sha3::{Digest, Sha3_256};
use userlib::*;

task_slot!(SP_CTRL, swd);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Version(u32, u32),
    None,
}

ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    loop {
        let id = SP_CTRL.get_task_id();
        let sp_ctrl = SpCtrl::from(id);

        if sp_ctrl.setup().is_err() {
            panic!();
        }

        match sp_access::active_image_version(&sp_ctrl) {
            Ok(f) => match f {
                Some((e, v)) => ringbuf_entry!(Trace::Version(e, v)),
                _ => (),
            },
            _ => (),
        }
        match sp_access::pending_image_version(&sp_ctrl) {
            Ok(f) => match f {
                Some((e, v)) => ringbuf_entry!(Trace::Version(e, v)),
                _ => (),
            },
            _ => (),
        }

        //sp_access::bank_erase(&sp_ctrl).unwrap();

        if sys_recv_closed(&mut [], 1, TaskId::KERNEL).is_err() {
            panic!();
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/expected.rs"));
