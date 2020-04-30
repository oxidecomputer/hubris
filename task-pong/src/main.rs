#![no_std]
#![no_main]

use userlib::*;
use zerocopy::AsBytes;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const RCC: Task = SELF;

#[export_name = "main"]
pub fn main() -> ! {
    const TIMER_NOTIFICATION: u32 = 1;
    const INTERVAL: u64 = 100;
    const SUCCESS_RESPONSE: u32 = 0;

    turn_on_gpiod();
    set_up_leds();

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

fn turn_on_gpiod() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, 0);
    const ENABLE_CLOCK: u16 = 1;
    let gpiod_pnum = 3; // see bits in AHB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, gpiod_pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn set_up_leds() {
    let gpiod = unsafe {
        &*stm32f4::stm32f407::GPIOD::ptr()
    };
    gpiod.moder.modify(|_, w| {
        w.moder12().output().moder13().output()
    });
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
