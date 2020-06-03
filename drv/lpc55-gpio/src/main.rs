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

use zerocopy::AsBytes;
use userlib::*;

const OP_SET_DIR: u32 = 1;
const OP_SET_VAL: u32 = 2;
const OP_READ_VAL: u32 = 3;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

#[cfg(feature = "standalone")]
const SYSCON: Task = SELF;

#[repr(u32)]
enum ResponseCode {
    Success = 0,
    BadOp = 1,
    BadArg = 2,
}

#[export_name = "main"]
fn main() -> ! {
    turn_on_gpio_clocks();

    // Going from our GPIO number to the IOCON interface here
    // is an absolute nightmare right now because each field is
    // named.
    //let iocon = unsafe  { &*device::IOCON::ptr() };
    let gpio = unsafe  { &*device::GPIO::ptr() };

    // Field messages.
    let mmask = 0;  // we don't use notifications.
    let mut buffer : [u8; 2] = [0; 2];
    loop {
        let msginfo = sys_recv(&mut buffer, mmask);
        match msginfo.operation {
            OP_SET_DIR => {
                if msginfo.message_len != 2 {
                    sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                }
                let gpionum = buffer[0];
                let dir = buffer[1];
                if gpionum < 32 {
                    let mask = 1 << gpionum;
                    if dir == 0 {
                        gpio.dirclr[0].write(|w| unsafe { w.dirclrp().bits(mask) } );
                    } else {
                        gpio.dirset[0].write(|w| unsafe { w.dirsetp().bits(mask) } );
                    }
                } else if gpionum < 64 {
                    let mask = 1 << (gpionum - 32);
                    if dir == 0 {
                        gpio.dirclr[1].write(|w| unsafe { w.dirclrp().bits(mask) } );
                    } else {
                        gpio.dirset[1].write(|w| unsafe { w.dirsetp().bits(mask) } );
                    }

                } else {
                    sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                }
                sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
            }
            OP_SET_VAL => {
                if msginfo.message_len != 2 {
                    sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                }
                let gpionum = buffer[0];
                let val = buffer[1];
                if gpionum < 32 {
                    let mask = 1 << gpionum;
                    if val == 0 {
                        gpio.clr[0].write(|w| { unsafe { w.clrp().bits(mask) } });
                    } else {
                        gpio.set[0].write(|w| { unsafe { w.setp().bits(mask) } });
                    }
                } else if gpionum < 64 {
                    let mask = 1 << (gpionum - 32);
                    if val == 0 {
                        gpio.clr[1].write(|w| { unsafe { w.clrp().bits(mask) } });
                    } else {
                        gpio.set[1].write(|w| { unsafe { w.setp().bits(mask) } });
                    }
                } else {
                    sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                }
                sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);

            }
            OP_READ_VAL => {
                // Make sure the pin is set in digital mode before trying to use
                // this function otherwise it will not work!
                let gpionum = buffer[0];
                let mut val = false;
                if gpionum < 32 {
                    let mask = 1 << gpionum;
                    val = (gpio.pin[0].read().port().bits() & mask) == mask;
                } else if gpionum < 64 {
                    let mask = 1 << (gpionum - 32);
                    val = (gpio.pin[1].read().port().bits() & mask) == mask;
                } else {
                    sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                }
                if val {
                    sys_reply(msginfo.sender, ResponseCode::Success as u32, &[1]);
                } else {
                    sys_reply(msginfo.sender, ResponseCode::Success as u32, &[0]);
                }
            }
            _ => {
                // Unrecognized operation code
                sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]);
            }
        }
    }
}

fn turn_on_gpio_clocks() {
    let syscon_driver = TaskId::for_index_and_gen(SYSCON as usize, Generation::default());
    const ENABLE_CLOCK: u16 = 1;

    let iocon_num = 13;
    let (code, _) = userlib::sys_send(syscon_driver, ENABLE_CLOCK, iocon_num.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    let gpio0_num = 14;
    let (code, _) = userlib::sys_send(syscon_driver, ENABLE_CLOCK, gpio0_num.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    let gpio1_num = 15;
    let (code, _) = userlib::sys_send(syscon_driver, ENABLE_CLOCK, gpio1_num.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}
