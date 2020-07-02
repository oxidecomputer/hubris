#![no_std]
#![no_main]

use userlib::*;
use zerocopy::AsBytes;

#[cfg(feature = "standalone")]
const PEER: Task = SELF;

#[cfg(not(feature = "standalone"))]
const PEER: Task = Task::pong;

#[cfg(all(feature = "standalone", feature = "uart"))]
const UART: Task = SELF;

#[cfg(all(not(feature = "standalone"), feature="uart"))]
const UART: Task = Task::usart_driver;

#[cfg(not(feature = "standalone"))]
const USER_LEDS: Task = Task::user_leds;

#[cfg(feature = "standalone")]
const USER_LEDS: Task = SELF;

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

fn set_led() {
    let leds = TaskId::for_index_and_gen(USER_LEDS as usize, Generation::default());
    const ON: u16 = 1;
    let (code, _) = userlib::sys_send(leds, ON, 0u32.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

#[cfg(feature = "uart")]
fn uart_send(text: &[u8]) {
    let peer = TaskId::for_index_and_gen(UART as usize, Generation::default());

    const OP_WRITE: u16 = 1;
    let (code, _) = sys_send(peer, OP_WRITE, &[], &mut [], &[
        Lease::from(text),
    ]);
    assert_eq!(0, code);
}

#[cfg(not(feature = "uart"))]
fn uart_send(_: &[u8]) {
}
