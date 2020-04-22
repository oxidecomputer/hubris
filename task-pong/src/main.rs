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
    let gpiod = unsafe {
        &*stm32f4::stm32f407::GPIOD::ptr()
    };
    gpiod.bsrr.write(|w| w.br12().set_bit());
}

fn toggle_other_led() {
    let gpiod = unsafe {
        &*stm32f4::stm32f407::GPIOD::ptr()
    };
    if gpiod.odr.read().odr13().bit() {
        gpiod.bsrr.write(|w| w.br13().set_bit());
    } else {
        gpiod.bsrr.write(|w| w.bs13().set_bit());
    }
}
