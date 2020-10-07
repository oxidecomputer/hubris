#![no_std]
#![no_main]

use userlib::*;

#[export_name = "main"]
pub fn main() -> ! {
    defmt::debug!("Pong task starting!");

    const TIMER_NOTIFICATION: u32 = 1;
    const INTERVAL: u64 = 500;

    let mut response: u32 = 0;

    let user_leds = get_user_leds();

    let mut current = 0;
    let mut msg = [0; 16];
    let mut dl = INTERVAL;
    sys_set_timer(Some(dl), TIMER_NOTIFICATION);
    loop {
        let msginfo = sys_recv_open(&mut msg, TIMER_NOTIFICATION);

        if msginfo.sender != TaskId::KERNEL {
            defmt::debug!("PONG!");
            // We'll just assume this is a ping message and reply.
            sys_reply(msginfo.sender, response, &[]);
            response += 1;
        } else {
            defmt::debug!("toggling leds");
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
                    Err(drv_user_leds_api::LedError::NoSuchLed) => {
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
