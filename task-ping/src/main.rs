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
fn stackblow() {
    let c = [0xdeu8; 8192];
    uart_send(&c[0..1024]);
}

#[inline(never)]
fn execdata() {
    unsafe {
        let c = [0x4770u16]; // bx lr

        let mut val: u32 = core::mem::transmute(&c);

        // set the Thumb bit
        val |= 1;

        let f: extern "C" fn(&[u16]) = core::mem::transmute(val);
        f(&c);
    }
}

static BXLR: [u16; 1] = [0x4770u16];

#[inline(never)]
fn illop() {
    unsafe {
        // This should attempt to execute with the Thumb bit clear, so
        // should trap on an "illegal operation"
        let val: u32 = core::mem::transmute(&BXLR);
        asm!("bx r0", in("r0") val);
    }
}

#[inline(never)]
fn nullread() {
    unsafe {
        // 0 is not in a region we can access; memory fault
        (0 as *const u8).read_volatile();
    }
}

#[inline(never)]
fn nullexec() {
    unsafe {
        let val: u32 = 1;
        let f: extern "C" fn() = core::mem::transmute(val);
        f();
    }
}

#[inline(never)]
fn textoob() {
    unsafe {
        // fly off the end of our text -- which will either induce
        // a memory fault (end of MPU-provided region) or a bus error
        // (reading never-written flash on some MCUs/boards, e.g. LPC55)
        let mut val: u32 = core::mem::transmute(&main);

        loop {
            (val as *const u8).read_volatile();
            val += 1;
        }
    }
}

#[inline(never)]
fn stackoob() {
    let c = [0xdeu8; 16];

    unsafe {
        // fly off the end of our stack on inducing a memory fault
        let mut val: u32 = core::mem::transmute(&c);

        loop {
            (val as *const u8).read_volatile();
            val += 1;
        }
    }
}

#[inline(never)]
fn busfault() {
    unsafe {
        // unprivileged software reading CSFR is a bus error
        (0xe000ed28 as *const u32).read_volatile();
    }
}

#[inline(never)]
fn illinst() {
    unsafe {
        // an illegal instruction
        asm!("udf 0xde");
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
    let user_leds = get_user_leds();

    let peer = TaskId::for_index_and_gen(PEER as usize, Generation::default());
    const PING_OP: u16 = 1;
    const FAULT_EVERY: u32 = 100;

    let faultme = [
        nullread, nullexec, stackblow, textoob, execdata, illop, stackoob,
        busfault, illinst, divzero,
    ];

    let mut response = [0; 16];
    loop {
        uart_send(b"Ping!\r\n");
        // Signal that we're entering send:
        user_leds.led_on(0).unwrap();

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
