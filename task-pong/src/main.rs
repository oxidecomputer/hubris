#![no_std]
#![no_main]

extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics

use userlib::*;

#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    safe_main()
}

fn safe_main() -> ! {
    const TIMER_NOTIFICATION: u32 = 1;
    const INTERVAL: u64 = 100;
    const SUCCESS_RESPONSE: u32 = 0;

    let mut msg = [0; 16];
    let mut dl = INTERVAL;
    loop {
        let msginfo = sys_recv(
            &mut msg,
            TIMER_NOTIFICATION,
        );
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
            // TODO toggle an LED here
        }
    }
}
