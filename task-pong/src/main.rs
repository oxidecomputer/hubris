#![no_std]
#![no_main]

use userlib::*;

#[export_name = "main"]
pub fn main() -> ! {
    const TIMER_NOTIFICATION: u32 = 1;
    const INTERVAL: u64 = 500;

    let mut response: u32 = 0;

    let user_leds = get_user_leds();

    let mut msg = [0; 16];
    let mut dl = INTERVAL;
    sys_set_timer(Some(dl), TIMER_NOTIFICATION);
    loop {
        let msginfo = sys_recv_open(&mut msg, TIMER_NOTIFICATION);

        // Signal that we have received
        user_leds.led_off(0).unwrap();

        if msginfo.sender != TaskId::KERNEL {
            // We'll just assume this is a ping message and reply.
            sys_reply(msginfo.sender, response, &[]);
            response += 1;
        } else {
            // This is a notification message. We've only got one notification
            // enabled, so we know full well which it is without looking.
            dl += INTERVAL;
            sys_set_timer(Some(dl), TIMER_NOTIFICATION);
            user_leds.led_toggle(1).unwrap();
        }
    }
}

fn get_user_leds() -> drv_user_leds_api::UserLeds {
    #[cfg(not(feature = "standalone"))]
    const USER_LEDS: Task = Task::user_leds;

    #[cfg(feature = "standalone")]
    const USER_LEDS: Task = Task::anonymous;

    drv_user_leds_api::UserLeds::from(TaskId::for_index_and_gen(
        USER_LEDS as usize,
        Generation::default(),
    ))
}
