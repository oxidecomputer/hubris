//! A driver for the LPC55S6x GPIO
//!
//! GPIO is covered by two separate hardware blocks: GPIO which handles the
//! typical GPIO set low/set high and IOCON which handles the pin configuration.
//!
//! This driver depends on the SYSCON driver being available
//!
//! GPIOs are specified via PIO{0,1}_{0-31}. For these APIs the numbers are,
//! PIO0_{n} = n
//! PIO1_{n} = 32 + n
//!
//! # IPC protocol
//!
//! ## `set_dir` (1)
//!
//! Sets the direction of the corresponding GPIO number, 0 = input, 1 = output
//!
//! Request message format: two `u8` giving GPIO number and direction
//!
//! ## `set_val` (2)
//!
//! Sets the digital value (0 or 1) to the corresponding GPIO number. The
//! GPIO pin must be configured as GPIO and an output already.
//!
//! Request message format: two `u8` giving GPIO number and value
//!
//! ## `read_val` (3)
//!
//! Reads the digital value to the corresponding GPIO number. The GPIO
//! pin must be configured as GPIO and an input already.
//!
//! Request message format: single `u8` giving GPIO number
//! Returns: Digital value
//!

#![no_std]
#![no_main]

use lpc55_pac as device;

use hl;
use userlib::{FromPrimitive, *};
use zerocopy::AsBytes;

#[derive(FromPrimitive)]
enum Op {
    SetDir = 1,
    SetVal = 2,
    ReadVal = 3,
}

declare_task!(SYSCON, syscon_driver);

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
    turn_on_gpio_clocks();

    // Going from our GPIO number to the IOCON interface here
    // is an absolute nightmare right now because each field is
    // named.
    //let iocon = unsafe  { &*device::IOCON::ptr() };
    let gpio = unsafe { &*device::GPIO::ptr() };

    // Handler for received messages.
    let recv_handler = |op: Op, msg: hl::Message| -> Result<(), ResponseCode> {
        match op {
            Op::SetDir => {
                let (&[gpionum, dir], caller) =
                    msg.fixed::<[u8; 2], ()>().ok_or(ResponseCode::BadArg)?;
                let (idx, mask) = gpio_num_pin_mask(gpionum)?;
                if dir == 0 {
                    gpio.dirclr[idx]
                        .write(|w| unsafe { w.dirclrp().bits(mask) });
                } else {
                    gpio.dirset[idx]
                        .write(|w| unsafe { w.dirsetp().bits(mask) });
                }
                caller.reply(());
                Ok(())
            }
            Op::SetVal => {
                let (&[gpionum, val], caller) =
                    msg.fixed::<[u8; 2], ()>().ok_or(ResponseCode::BadArg)?;
                let (idx, mask) = gpio_num_pin_mask(gpionum)?;
                if val == 0 {
                    gpio.clr[idx].write(|w| unsafe { w.clrp().bits(mask) });
                } else {
                    gpio.set[idx].write(|w| unsafe { w.setp().bits(mask) });
                }
                caller.reply(());
                Ok(())
            }
            Op::ReadVal => {
                // Make sure the pin is set in digital mode before trying to
                // use this function otherwise it will not work!
                let (&gpionum, caller) =
                    msg.fixed::<u8, u8>().ok_or(ResponseCode::BadArg)?;
                let (idx, mask) = gpio_num_pin_mask(gpionum)?;
                let val = (gpio.pin[idx].read().port().bits() & mask) == mask;
                caller.reply(val as u8);
                Ok(())
            }
        }
    };

    // Field messages.
    let mut buffer: [u8; 2] = [0; 2];
    loop {
        hl::recv_without_notification(&mut buffer, recv_handler);
    }
}

fn gpio_num_pin_mask(gpionum: u8) -> Result<(usize, u32), ResponseCode> {
    if gpionum >= 64 {
        return Err(ResponseCode::BadArg);
    }
    let (idx, offset) = if gpionum < 32 { (0, 0) } else { (1, 32) };
    Ok((idx, 1 << (gpionum - offset)))
}

fn turn_on_gpio_clocks() {
    let syscon_driver = get_task_id(SYSCON);
    const ENABLE_CLOCK: u16 = 1;

    let iocon_num = 13;
    let (code, _) = userlib::sys_send(
        syscon_driver,
        ENABLE_CLOCK,
        iocon_num.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(code, 0);

    let gpio0_num = 14;
    let (code, _) = userlib::sys_send(
        syscon_driver,
        ENABLE_CLOCK,
        gpio0_num.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(code, 0);

    let gpio1_num = 15;
    let (code, _) = userlib::sys_send(
        syscon_driver,
        ENABLE_CLOCK,
        gpio1_num.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(code, 0);
}
