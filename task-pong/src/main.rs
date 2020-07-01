#![no_std]
#![no_main]

#[cfg(armv8m)]
use lpc55_pac as device;

use userlib::*;
use zerocopy::AsBytes;

#[cfg(all(not(feature = "standalone"), armv7m))]
const RCC: Task = Task::rcc_driver;

#[cfg(all(feature = "standalone", armv7m))]
const RCC: Task = SELF;

#[cfg(all(not(feature = "standalone"), armv8m))]
const GPIO: Task = Task::gpio_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(all(feature = "standalone", armv8m))]
const GPIO: Task = SELF;

#[export_name = "main"]
pub fn main() -> ! {
    const TIMER_NOTIFICATION: u32 = 1;
    const INTERVAL: u64 = 100;
    const SUCCESS_RESPONSE: u32 = 0;

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

#[cfg(all(armv7m, feature = "stm32f4"))]
fn turn_on_gpio() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());
    const ENABLE_CLOCK: u16 = 1;
    let gpiod_pnum = 3; // see bits in AHB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, gpiod_pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

#[cfg(all(armv7m, feature = "stm32h7"))]
fn turn_on_gpio() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());
    const ENABLE_CLOCK: u16 = 1;
    let gpiog_pnum = 102; // AHB4ENR=96 + 6
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, gpiog_pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

#[cfg(all(armv7m, feature = "stm32f4"))]
fn set_up_leds() {
    turn_on_gpio();
    let gpiod = unsafe {
        &*stm32f4::stm32f407::GPIOD::ptr()
    };
    gpiod.moder.modify(|_, w| {
        w.moder12().output().moder13().output()
    });
}

#[cfg(all(armv7m, feature = "stm32h7"))]
fn set_up_leds() {
    turn_on_gpio();
    let gpiog = unsafe {
        &*stm32h7::stm32h7b3::GPIOG::ptr()
    };
    gpiog.moder.modify(|_, w| {
        w.moder2().output().moder11().output()
    });
}

#[cfg(armv8m)]
fn set_up_leds() {
    let gpio_driver = TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    const SET_DIR: u16 = 1;

    // Ideally this would be done in another driver but given what svd2rust
    // generates it's a nightmare to do this via pin indexing only and
    // also have some degree of safety. If the pins aren't in digital mode
    // the GPIO toggling will work but reading the value won't
    let iocon = unsafe  { &*device::IOCON::ptr() };
    iocon.pio1_4.modify( |_, w| w.digimode().digital() );
    iocon.pio1_6.modify( |_, w| w.digimode().digital() );

    // red led
    let (code, _) = userlib::sys_send(gpio_driver, SET_DIR, &[38, 1], &mut [], &[]);
    assert_eq!(code, 0);

    // blue led
    let (code, _) = userlib::sys_send(gpio_driver, SET_DIR, &[36, 1], &mut [], &[]);
    assert_eq!(code, 0);
}

#[cfg(all(armv7m, feature = "stm32f4"))]
fn clear_led() {
    let gpiod = unsafe {
        &*stm32f4::stm32f407::GPIOD::ptr()
    };
    gpiod.bsrr.write(|w| w.br12().set_bit());
}

#[cfg(all(armv7m, feature = "stm32h7"))]
fn clear_led() {
    let gpiog = unsafe {
        &*stm32h7::stm32h7b3::GPIOG::ptr()
    };
    gpiog.bsrr.write(|w| w.br11().set_bit());
}

#[cfg(armv8m)]
fn clear_led() {
    let gpio_driver = TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    const SET_VAL: u16 = 2;
    // Blue LED
    let (code, _) = userlib::sys_send(gpio_driver, SET_VAL, &[36, 1], &mut [], &[]);
    assert_eq!(code, 0);
}

#[cfg(all(armv7m, feature = "stm32f4"))]
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

#[cfg(all(armv7m, feature = "stm32h7"))]
fn toggle_other_led() {
    let gpiog = unsafe {
        &*stm32h7::stm32h7b3::GPIOG::ptr()
    };
    if gpiog.odr.read().odr2().bit() {
        gpiog.bsrr.write(|w| w.br2().set_bit());
    } else {
        gpiog.bsrr.write(|w| w.bs2().set_bit());
    }
}


#[cfg(armv8m)]
fn toggle_other_led() {
    let gpio_driver = TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    const SET_VAL: u16 = 2;
    const READ_VAL: u16 = 3;
    let mut val : u32 = 0;

    let (code, _) = userlib::sys_send(gpio_driver, READ_VAL, &[38], val.as_bytes_mut(), &[]);
    assert_eq!(code, 0);

    if val == 1 {
        let (code, _) = userlib::sys_send(gpio_driver, SET_VAL, &[38, 0], &mut [], &[]);
        assert_eq!(code, 0);
    } else {
        let (code, _) = userlib::sys_send(gpio_driver, SET_VAL, &[38, 1], &mut [], &[]);
        assert_eq!(code, 0);
    }
}
