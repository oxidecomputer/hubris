// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use userlib::*;

task_slot!(USER_LEDS, user_leds);

#[export_name = "main"]
pub fn main() -> ! {
    const TIMER_NOTIFICATION: u32 = 1;
    const INTERVAL: u64 = 500;

    let mut response: u32 = 0;

    let user_leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());

    let mut current = 0;
    let mut msg = [0; 16];
    let mut dl = INTERVAL;
    sys_set_timer(Some(dl), TIMER_NOTIFICATION);
    loop {
        let msginfo = sys_recv_open(&mut msg, TIMER_NOTIFICATION);

        if msginfo.sender != TaskId::KERNEL {
            // We'll just assume this is a ping message and reply.
            sys_reply(msginfo.sender, response, &[]);
            response += 1;
        } else {
            // This is a notification message. We've only got one notification
            // enabled, so we know full well which it is without looking.
            dl += INTERVAL;
            sys_set_timer(Some(dl), TIMER_NOTIFICATION);

            // Toggle the current LED -- and if we've run out, start over
            loop {
                match user_leds.led_toggle(current >> 1) {
                    Ok(_) => {
                        current = current + 1;
                        break;
                    }
                    Err(drv_user_leds_api::LedError::NotPresent) => {
                        current = 0;
                    }
                    _ => {
                        panic!("unhandled Led error");
                    }
                };
            }
        }
    }
}
