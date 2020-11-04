//! "Assistant" task for testing interprocess interactions.

#![no_std]
#![no_main]
#![feature(asm)]

use userlib::*;
use test_api::*;
use zerocopy::AsBytes;

#[inline(never)]
fn badread(arg: u32) {
    unsafe {
        (arg as *const u8).read_volatile();
    }
}

fn panic(_arg: u32) {
    panic!("wow this blew up, here's my soundcloud");
}

#[inline(never)]
fn stackblow(_arg: u32) {
    let c = [0xdeu8; 8192];
    panic!("val is {}", c[2000]);
}

#[inline(never)]
fn execdata(_arg: u32) {
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
fn illop(_arg: u32) {
    unsafe {
        // This should attempt to execute with the Thumb bit clear, so
        // should trap on an "illegal operation"
        let val: u32 = core::mem::transmute(&BXLR);
        asm!("bx r0", in("r0") val);
    }
}

#[inline(never)]
fn badexec(_arg: u32) {
    unsafe {
        let val: u32 = 1;
        let f: extern "C" fn() = core::mem::transmute(val);
        f();
    }
}

#[inline(never)]
fn textoob(_arg: u32) {
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
fn stackoob(_arg: u32) {
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
fn busfault(_arg: u32) {
    unsafe {
        // unprivileged software reading CSFR is a bus error
        (0xe000ed28 as *const u32).read_volatile();
    }
}

#[inline(never)]
fn illinst(_arg: u32) {
    unsafe {
        // an illegal instruction
        asm!("udf 0xde");
    }
}

#[inline(never)]
fn divzero(_arg: u32) {
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
    sys_log!("assistant starting");
    let mut buffer = [0; 4];
    let mut last_reply = 0u32;
    let mut stored_value = 0;
    let mut borrow_buffer = [0u8; 16];

    let fatalops = [
        (AssistOp::BadMemory, badread as fn(u32)),
        (AssistOp::Panic, panic),
        (AssistOp::DivZero, divzero),
        (AssistOp::StackOverflow, stackblow),
        (AssistOp::ExecData, execdata),
        (AssistOp::IllegalOperation, illop),
        (AssistOp::BadExec, badexec),
        (AssistOp::TextOutOfBounds, textoob),
        (AssistOp::StackOutOfBounds, stackoob),
        (AssistOp::BusError, busfault),
        (AssistOp::IllegalInstruction, illinst),
    ];

    loop {
        hl::recv_without_notification(
            &mut buffer,
            |op, msg| -> Result<(), u32> {
                // Every incoming message uses the same payload type: it's
                // always u32 -> u32.
                let (msg, caller) = msg.fixed::<u32, u32>().ok_or(1u32)?;

                match op {
                    AssistOp::JustReply => {
                        // To demonstrate comprehension, we perform a some
                        // arithmetic on the message and send it back.
                        caller.reply(!msg);
                    }
                    AssistOp::SendBack => {
                        // Immediately resume the caller...
                        let task_id = caller.task_id();
                        caller.reply(*msg);
                        // ...and then send them a message back, recording any
                        // reply as last_reply
                        sys_send(
                            task_id,
                            42,
                            &msg.to_le_bytes(),
                            last_reply.as_bytes_mut(),
                            &[],
                        );
                        // Ignore the result.
                    }
                    AssistOp::LastReply => {
                        caller.reply(last_reply);
                    }
                    AssistOp::Store => {
                        caller.reply(stored_value);
                        stored_value = *msg;
                    }
                    AssistOp::SendBackWithLoans => {
                        // Immediately resume the caller...
                        let task_id = caller.task_id();
                        caller.reply(*msg);
                        // ...and then send them a message back, recording any
                        // reply as last_reply
                        sys_send(
                            task_id,
                            42,
                            &msg.to_le_bytes(),
                            last_reply.as_bytes_mut(),
                            &[
                                // Lease 0 is writable.
                                Lease::from(&mut borrow_buffer[..]),
                                // Lease 1 is not.
                                Lease::from(&b"hello"[..]),
                            ],
                        );
                        // Ignore the result.
                    }
                    _ => {
                        // Anything else should be fatal
                        for (which, func) in &fatalops {
                            if *which != op {
                                continue;
                            }

                            caller.reply(0);
                            func(*msg);
                            panic!("unexpectedly survived {:?}", op);
                        }

                        panic!("unmatched operation {:?}", op);
                    }
                }

                Ok(())
            },
        );
    }
}
