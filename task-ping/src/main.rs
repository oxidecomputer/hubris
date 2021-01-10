#![no_std]
#![no_main]
#![feature(asm)]

use userlib::*;

#[cfg(feature = "standalone")]
const PEER: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const PEER: Task = Task::pong;

#[cfg(all(feature = "standalone", feature = "uart"))]
const UART: Task = Task::anonymous;

#[cfg(all(not(feature = "standalone"), feature = "uart"))]
const UART: Task = Task::usart_driver;

#[inline(never)]
fn nullread() {
    unsafe {
        // 0 is not in a region we can access; memory fault
        core::ptr::null::<u8>().read_volatile();
    }
}

#[inline(never)]
fn divzero() {
    unsafe {
        // Divide by 0
        let p: u32 = 123;
        let q: u32 = 0;
        let _res: u32;
        asm!("udiv r2, r1, r0", in("r1") p, in("r0") q, out("r2") _res);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let peer = TaskId::for_index_and_gen(PEER as usize, Generation::default());
    const PING_OP: u16 = 1;
    const FAULT_EVERY: u32 = 100;

    let faultme = [nullread, divzero];

    let mut response = [0; 16];
    loop {
        uart_send(b"Ping!\r\n");

        let (code, _len) =
            sys_send(peer, PING_OP, b"hello", &mut response, &[]);

        if code % FAULT_EVERY != 0 {
            continue;
        }

        let op = (code / FAULT_EVERY) as usize % faultme.len();
        faultme[op]();
        sys_panic(b"unexpected non-fault!");
    }
}

#[cfg(feature = "uart")]
fn uart_send(text: &[u8]) {
    let peer = TaskId::for_index_and_gen(UART as usize, Generation::default());

    const OP_WRITE: u16 = 1;
    let (code, _) =
        sys_send(peer, OP_WRITE, &[], &mut [], &[Lease::from(text)]);
    assert_eq!(0, code);
}

#[cfg(not(feature = "uart"))]
fn uart_send(_: &[u8]) {}
