#![no_std]
#![no_main]

use cortex_m_semihosting::hprintln;
use userlib::*;
use zerocopy::AsBytes;

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

#[export_name = "main"]
fn main() -> ! {
    // Ensure our buffer is aligned properly for a u32 by declaring it as one.
    let mut buffer = [0u32; 1];
    let mut count = 0;

    loop {
        if count >= 2 {
            panic!("wow this blew up, here's my soundcloud");
        } else {
            hprintln!("task2: not blowing up yet").ok();
        }

        // NOTE: you need to put code here before running this! Otherwise LLVM
        // will turn this into a single undefined instruction.
        hprintln!("task2: time to recv").ok();
        hl::recv_without_notification(
            buffer.as_bytes_mut(),
            |_op: u16, msg| -> Result<(), ResponseCode> {
                hprintln!("task2: got message!").ok();
                let (_msg, caller) =
                    msg.fixed::<(), ()>().ok_or(ResponseCode::BadArg)?;

                count += 1;

                caller.reply(());
                hprintln!("task2: replied").ok();
                Ok(())
            },
        );
        hl::sleep_for(5);
    }
}
