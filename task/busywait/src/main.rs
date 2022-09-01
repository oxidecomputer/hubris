// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use userlib::*;

#[export_name = "main"]
fn main() -> ! {
    loop {
        hl::recv_without_notification(
            &mut [],
            |_op: u32, msg| -> Result<(), u32> {
                let ((), caller) = msg.fixed::<(), ()>().ok_or(1_u32)?;
                let start = sys_get_timer().now;
                loop {
                    if sys_get_timer().now - start > 5_000 {
                        break;
                    }
                }
                caller.reply(());
                Ok(())
            },
        );
    }
}
