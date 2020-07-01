#![no_std]
#![no_main]

use userlib::*;

#[cfg(feature = "standalone")]
const PEER: Task = SELF;

#[cfg(not(feature = "standalone"))]
const PEER: Task = Task::pong;

#[cfg(any(feature = "standalone", feature = "stm32h7"))]
const UART: Task = SELF;

#[cfg(all(not(feature = "standalone"), not(feature="stm32h7")))]
const UART: Task = Task::usart_driver;

#[cfg(all(not(feature = "standalone"), armv8m))]
const GPIO: Task = Task::gpio_driver;

#[cfg(all(feature = "standalone", armv8m))]
const GPIO: Task = SELF;

#[export_name = "main"]
fn main() -> ! {
    let peer = TaskId::for_index_and_gen(PEER as usize, Generation::default());
    const PING_OP: u16 = 1;
    let mut response = [0; 16];
    let mut iterations = 0usize;
    loop {
        uart_send(b"Ping!\r\n");
        // Signal that we're entering send:
        set_led();

        iterations += 1;
        if iterations == 1000 {
            // mwa ha ha ha
            unsafe { (0 as *const u8).read_volatile(); }
        }

        let (_code, _len) = sys_send(
            peer,
            PING_OP,
            b"hello",
            &mut response,
            &[],
        );
    }
}

#[cfg(armv8m)]
fn set_led() {
    let gpio_driver = TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    const SET_VAL: u16 = 2;
    // Blue LED
    let (code, _) = userlib::sys_send(gpio_driver, SET_VAL, &[36, 0], &mut [], &[]);
    assert_eq!(0, code);
}

#[cfg(all(armv7m, feature = "stm32f4"))]
fn set_led() {
    let gpiod = unsafe {
        &*stm32f4::stm32f407::GPIOD::ptr()
    };
    gpiod.bsrr.write(|w| w.bs12().set_bit());
}

#[cfg(all(armv7m, feature = "stm32h7"))]
fn set_led() {
    let gpiog = unsafe {
        &*stm32h7::stm32h7b3::GPIOG::ptr()
    };
    gpiog.bsrr.write(|w| w.bs11().set_bit());
}

#[cfg(not(feature = "stm32h7"))] // TODO
fn uart_send(text: &[u8]) {
    let peer = TaskId::for_index_and_gen(UART as usize, Generation::default());

    const OP_WRITE: u16 = 1;
    let (code, _) = sys_send(peer, OP_WRITE, &[], &mut [], &[
        Lease::from(text),
    ]);
    assert_eq!(0, code);
}

#[cfg(feature = "stm32h7")] // TODO
fn uart_send(_: &[u8]) {
}
