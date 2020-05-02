#![no_std]
#![no_main]
#![feature(llvm_asm)]

use userlib::*;

#[cfg(feature = "standalone")]
const PEER: Task = SELF;

#[cfg(not(feature = "standalone"))]
const PEER: Task = Task::pong;

#[cfg(feature = "standalone")]
const UART: Task = SELF;

#[cfg(not(feature = "standalone"))]
const UART: Task = Task::usart_driver;

#[export_name = "main"]
fn main() -> ! {
    let peer = TaskId::for_index_and_gen(PEER as usize, 0);
    const PING_OP: u16 = 1;
    let mut response = [0; 16];
    loop {
        uart_send(b"Ping!\r\n");
        // Signal that we're entering send:
        set_led();

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
    let gpiod = unsafe {
        &*stm32f4::stm32f407::GPIOD::ptr()
    };
    gpiod.bsrr.write(|w| w.bs12().set_bit());
}

fn uart_send(text: &[u8]) {
    let peer = TaskId::for_index_and_gen(UART as usize, 0);

    const OP_WRITE: u16 = 1;
    let (code, _) = sys_send(peer, OP_WRITE, &[], &mut [], &[
        Lease::from(text),
    ]);
    assert_eq!(0, code);
}
