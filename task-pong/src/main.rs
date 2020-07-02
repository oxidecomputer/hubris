#![no_std]
#![no_main]

use userlib::*;
use zerocopy::AsBytes;

#[cfg(not(feature = "standalone"))]
const USER_LEDS: Task = Task::user_leds;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const USER_LEDS: Task = SELF;

#[export_name = "main"]
pub fn main() -> ! {
    const TIMER_NOTIFICATION: u32 = 1;
    const INTERVAL: u64 = 100;
    const SUCCESS_RESPONSE: u32 = 0;

    let mut msg = [0; 16];
    let mut dl = INTERVAL;
    sys_set_timer(Some(dl), TIMER_NOTIFICATION);
    loop {
        let msginfo = sys_recv(
            &mut msg,
            TIMER_NOTIFICATION,
        );

        // Signal that we have received
        clear_led();

        if msginfo.sender != TaskId::KERNEL {
            // We'll just assume this is a ping message and reply.
            sys_reply(
                msginfo.sender,
                SUCCESS_RESPONSE,
                &[],
            );
        } else {
            // This is a notification message. We've only got one notification
            // enabled, so we know full well which it is without looking.
            dl += INTERVAL;
            sys_set_timer(Some(dl), TIMER_NOTIFICATION);
            toggle_other_led();
        }
    }
}

fn clear_led() {
    let leds = TaskId::for_index_and_gen(USER_LEDS as usize, Generation::default());
    const OFF: u16 = 2;
    let (code, _) = userlib::sys_send(leds, OFF, 0u32.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn toggle_other_led() {
    let leds = TaskId::for_index_and_gen(USER_LEDS as usize, Generation::default());
    const TOGGLE: u16 = 3;
    let (code, _) = userlib::sys_send(leds, TOGGLE, 1u32.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}
