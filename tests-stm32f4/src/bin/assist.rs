//! "Assistant" task for testing interprocess interactions.

#![no_std]
#![no_main]

use userlib::*;
use zerocopy::AsBytes;

#[derive(FromPrimitive)]
enum Op {
    JustReply = 0,
    SendBack = 1,
    LastReply = 2,
    Crash = 3,
    Panic = 4,
    Store = 5,
    SendBackWithLoans = 6,
}

#[export_name = "main"]
fn main() -> ! {
    sys_log!("assistant starting");
    let mut buffer = [0; 4];
    let mut last_reply = 0u32;
    let mut stored_value = 0;
    let mut borrow_buffer = [0u8; 16];

    loop {
        hl::recv_without_notification(
            &mut buffer,
            |op, msg| -> Result<(), u32> {
                // Every incoming message uses the same payload type: it's
                // always u32 -> u32.
                let (msg, caller) = msg.fixed::<u32, u32>()
                    .ok_or(1u32)?;

                match op {
                    Op::JustReply => {
                        // To demonstrate comprehension, we perform a some
                        // arithmetic on the message and send it back.
                        caller.reply(!msg);
                    },
                    Op::SendBack => {
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
                    },
                    Op::LastReply => {
                        caller.reply(last_reply);
                    },
                    Op::Crash => {
                        caller.reply(0);
                        unsafe {
                            (*msg as *const u8).read_volatile();
                        }
                        panic!("Stray pointer access did not crash! \
                            Is memory protection working?!");
                    },
                    Op::Panic => {
                        caller.reply(0);
                        panic!("blarg i am dead")
                    },
                    Op::Store => {
                        caller.reply(stored_value);
                        stored_value = *msg;
                    },
                    Op::SendBackWithLoans => {
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
                    },
                }

                Ok(())
            },
        );
    }
}
