// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! "Assistant" task for testing interprocess interactions.

#![no_std]
#![no_main]
#![feature(asm)]

use hubris_num_tasks::NUM_TASKS;
use test_api::*;
use userlib::*;
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
fn badexec(arg: u32) {
    unsafe {
        let val: u32 = arg | 1;
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
        let mut val: u32 = core::mem::transmute(main as fn() -> _);

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
#[cfg(any(armv7m, armv8m))]
fn divzero(_arg: u32) {
    unsafe {
        // Divide by 0
        let p: u32 = 123;
        let q: u32 = 0;
        let _res: u32;
        asm!("udiv r2, r1, r0", in("r1") p, in("r0") q, out("r2") _res);
    }
}

#[inline(never)]
#[cfg(any(armv7m, armv8m))]
fn eat_some_pi(highregs: bool) {
    let mut pi = [0x40490fdb; 16];

    for i in 1..16 {
        pi[i] += i << 23;
    }

    unsafe {
        if !highregs {
            asm!("vldm {0}, {{s0-s15}}", in(reg) &pi);
        } else {
            asm!("vldm {0}, {{s16-s31}}", in(reg) &pi);
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    sys_log!("assistant starting");
    let mut buffer = [0; 4];
    let mut last_reply = 0u32;
    let mut stored_value = 0;
    let mut borrow_buffer = [0u8; 16];
    let mut posted_bits = 0;

    let fatalops = [
        (AssistOp::BadMemory, badread as fn(u32)),
        (AssistOp::Panic, panic),
        #[cfg(any(armv7m, armv8m))]
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

    const ALL_NOTIFICATIONS: u32 = !0;
    loop {
        hl::recv(
            &mut buffer,
            ALL_NOTIFICATIONS,
            &mut posted_bits,
            |posted_bits, notify_bits| {
                // Just record any notifications so they can be read back out.
                *posted_bits |= notify_bits;
            },
            |posted_bits, op, msg| -> Result<(), u32> {
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
                    #[cfg(any(armv7m, armv8m))]
                    AssistOp::EatSomePi => {
                        eat_some_pi(*msg > 0);
                        caller.reply(*msg);
                    }
                    #[cfg(any(armv7m, armv8m))]
                    AssistOp::PiAndDie => {
                        eat_some_pi(false);
                        eat_some_pi(true);
                        caller.reply(0);
                        illinst(0);
                        panic!("unexpectedly survived {:?}", op);
                    }

                    AssistOp::ReadTaskStatus => {
                        caller.reply(0);
                        let _ = kipc::read_task_status(*msg as usize);
                    }

                    AssistOp::FaultTask => {
                        caller.reply(0);
                        let _ = kipc::fault_task(*msg as usize);
                    }

                    AssistOp::RestartTask => {
                        caller.reply(0);
                        let _ = kipc::restart_task(*msg as usize, true);
                    }

                    AssistOp::RefreshTaskIdOffByOne => {
                        caller.reply(0);
                        let _ = sys_refresh_task_id(TaskId::for_index_and_gen(
                            NUM_TASKS,
                            Generation::default(),
                        ));
                        panic!("unexpectedly survived {:?}", op);
                    }

                    AssistOp::RefreshTaskIdOffByMany => {
                        caller.reply(0);
                        let _ = sys_refresh_task_id(TaskId::for_index_and_gen(
                            usize::MAX,
                            Generation::default(),
                        ));
                        panic!("unexpectedly survived {:?}", op);
                    }
                    AssistOp::ReadNotifications => {
                        caller.reply(core::mem::replace(posted_bits, 0));
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
