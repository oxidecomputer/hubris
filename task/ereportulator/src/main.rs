// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! # BEHOLD THE EREPORTULATOR!
//!
//! stupid ereport demo task
//!
#![no_std]
#![no_main]

use task_packrat_api::Packrat;
task_slot!(PACKRAT, packrat);

use minicbor::Encoder;
use userlib::{sys_recv_notification, task_slot, UnwrapLite};

#[export_name = "main"]
fn main() -> ! {
    let packrat = Packrat::from(PACKRAT.get_task_id());

    let mut buf = [0u8; 256];

    let encoded_len = {
        let c = minicbor::encode::write::Cursor::new(&mut buf[..]);
        let mut encoder = Encoder::new(c);
        encoder
            .begin_map()
            .unwrap_lite()
            .str("k")
            .unwrap_lite()
            .str("TEST EREPORT PLS IGNORE")
            .unwrap_lite()
            .str("badness")
            .unwrap_lite()
            .u32(10000)
            .unwrap_lite()
            .str("msg")
            .unwrap_lite()
            .str("im dead")
            .unwrap_lite()
            .end()
            .unwrap_lite();

        encoder.into_writer().position()
    };

    packrat.deliver_ereport(&buf[..encoded_len]);

    loop {
        // now die!
        sys_recv_notification(0);
        // TODO(eliza): eventually it might be a lil nicer if we had an IPC
        // interface for sending an ereport, so we could trigger this multiple
        // times from humility...
    }
}
