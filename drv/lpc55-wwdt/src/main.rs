//! Windowed Watchdog Timer driver
//!
//! The Windowed Watchdog timer's job is to reset the system if things go awry.
//! It does this by counting down, and if it hits zero, the system resets. In order
//! to not hit zero, another task must feed it every so often.

#![no_std]
#![no_main]

use lpc55_pac as device;

use userlib::*;
use zerocopy::AsBytes;

use drv_lpc55_syscon_api::{Peripheral, Syscon};

#[cfg(feature = "standalone")]
const SYSCON: Task = SELF;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

#[derive(FromPrimitive)]
enum Op {
    /// Feed the watchdog
    Feed = 1,
}

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
    let syscon =
        TaskId::for_index_and_gen(SYSCON as usize, Generation::default());
    let syscon = Syscon::from(syscon);

    let wwdt = unsafe { &*device::WWDT::ptr() };

    syscon.enable_clock(Peripheral::Wwdt);

    syscon.leave_reset(Peripheral::Wwdt);

    // write 0 to wdtof so that if it's 1 on next boot, we know it's the wwdt that caused the reset
    //
    // TODO: right now we don't check wdtof, but we will want to eventually, and we should do so after
    // leaving the reset above, but before we overwrite its value here
    wwdt.mod_.write(|w| w.wdtof().bit(false));

    // tc is the "timer constant," aka, where we start counting down from. It's 24-bit.
    //
    // TODO: what's the real value we want here?
    wwdt.tc.write(|w| unsafe { w.bits(0x01_0000) });

    wwdt.mod_
        .write(|w| w.wden().run().wdreset().reset().wdint().set_bit());

    // set windowing to max, since we don't intend to use it
    wwdt.window.write(|w| unsafe { w.window().bits(0xFF_FFFF) });

    // set the interrupt warning value to zero, since we don't intend to use it
    wwdt.warnint.write(|w| unsafe { w.warnint().bits(0x0) });

    // last step of the process: feed the watchdog
    feed(wwdt);

    // TODO: we may want to protect the watchdog value eventually, but not for now

    // Ensure our buffer is aligned properly for a u32 by declaring it as one.
    let mut buffer = [0u32; 1];

    loop {
        hl::recv_without_notification(
            buffer.as_bytes_mut(),
            |op, msg| -> Result<(), ResponseCode> {
                let (_msg, caller) =
                    msg.fixed::<(), ()>().ok_or(ResponseCode::BadArg)?;

                match op {
                    Op::Feed => {
                        feed(wwdt);
                    }
                }

                caller.reply(());
                Ok(())
            },
        );
    }
}

/// Feeds the wwdt
///
/// This sequence should not be interrupted, but with the current design of
/// hubris, it is not interruptable, since we don't write to the feed register
/// anywhere but here.
fn feed(wwdt: &lpc55_pac::wwdt::RegisterBlock) {
    wwdt.feed.write(|w| unsafe { w.feed().bits(0xAA) });
    wwdt.feed.write(|w| unsafe { w.feed().bits(0x55) });
}
