//! A driver for the LPC55S6x SYSCON block
//!
//! This driver is responsible for clocks (peripherals and PLLs), systick
//! callibration, memory remapping, id registers. Most drivers will be
//! interested in the clock bits.
//!
//! # IPC protocol
//! 
//! Peripheral bit numbers per the LPC55 manual section 4.5 (for the benefit of
//! the author writing this driver who hates having to look these up. Double
//! check these later!)
//!
//! ROM = 1
//! SRAM_CTRL1 = 3
//! SRAM_CTRL2 = 4
//! SRAM_CTRL3 = 5
//! SRAM_CTRL4 = 6
//! FLASH = 7
//! FMC = 8
//! MUX = 11
//! IOCON = 13
//! GPIO0 = 14
//! GPIO1 = 15
//! PINT = 18
//! GINT = 19
//! DMA0 = 20
//! CRCGEN = 21
//! WWDT = 22
//! RTC = 23
//! MAILBOX = 26
//! ADC = 27
//! MRT = 32 + 0 = 32
//! OSTIMER = 32 + 1 = 33
//! SCT = 32 + 2 = 34
//! UTICK = 32 + 10 = 42
//! FC0 = 32 + 11 = 43
//! FC1 = 32 + 12 = 44
//! FC2 = 32 + 13 = 45
//! FC3 = 32 + 14 = 46
//! FC4 = 32 + 15 = 47
//! FC5 = 32 + 16 = 48
//! FC6 = 32 + 17 = 49
//! FC7 = 32 + 18 = 50
//! TIMER2 = 32 + 22 = 54
//! USB0_DEV = 32 + 25 = 57
//! TIMER0 = 32 + 26 = 58
//! TIMER1 = 32 + 27 = 59
//! DMA1 = 32 + 32 + 1 = 65
//! COMP = 32 + 32 + 2 = 66
//! SDIO = 32 + 32 + 3 = 67
//! USB1_HOST = 32 + 32 + 4 = 68
//! USB1_DEV = 32 + 32 + 5 = 69
//! USB1_RAM = 32 + 32 + 6 = 70
//! USB1_PHY = 32 + 32 + 7 = 71
//! FREQME = 32 + 32 + 8 = 72
//! RNG = 32 + 32 + 13 = 77
//! SYSCTL =  32 + 32 + 15 = 79
//! USB0_HOSTM = 32 + 32 + 16 = 80
//! USB0_HOSTS = 32 + 32 + 17 = 81
//! HASH_AES = 32 + 32 + 18 = 82
//! PQ = 32 + 32 + 19 = 83
//! PLULUT = 32 + 32 + 20 = 84
//! TIMER3 = 32 + 32 + 21 = 85
//! TIMER4 = 32 + 32 + 22 = 86
//! PUF = 32 + 32 + 23 = 87
//! CASPER = 32 + 32 + 24 = 88
//! ANALOG_CTRL = 32 + 32 + 27 = 91
//! HS_LSPI = 32 + 32 + 28 = 92
//! GPIO_SEC = 32 + 32 + 29 = 93
//! GPIO_SEC_INT = 32 + 32 + 30 = 94
//!
//! ## `enable_clock` (1)
//!
//! Requests that the clock to a peripheral be turned on.
//!
//! Peripherals are numbered by bit number in the SYSCON registers
//!
//! - PRESETCTRL0[31:0] are indices 31-0.
//! - PRESETCTRL1[31:0] are indices 63-32.
//! - PRESETCTRL2[31:0] are indices 64-96.
//!
//! Request message format: single `u32` giving peripheral index as described
//! above.
//!
//! ## `disable_clock` (2)
//!
//! Requests that the clock to a peripheral be turned off.
//!
//! Request message format: single `u32` giving peripheral index as described
//! for `enable_clock`.
//!
//! ## `enter_reset` (3)
//!
//! Requests that the reset line to a peripheral be asserted.
//!
//! Request message format: single `u32` giving peripheral index as described
//! for `enable_clock`.
//!
//! ## `leave_reset` (4)
//!
//! Requests that the reset line to a peripheral be deasserted.
//!
//! Request message format: single `u32` giving peripheral index as described
//! for `enable_clock`.

#![no_std]
#![no_main]

use lpc55_pac as device;

use zerocopy::AsBytes;
use userlib::*;

const OP_ENABLE_CLOCK: u32 = 1;
const OP_DISABLE_CLOCK: u32 = 2;
const OP_ENTER_RESET: u32 = 3;
const OP_LEAVE_RESET: u32 = 4;

#[repr(u32)]
enum ResponseCode {
    Success = 0,
    BadOp = 1,
    BadArg = 2,
}

#[export_name = "main"]
fn main() -> ! {
    // From the manual:
    //
    // The clock to the SYSCON block is
    // always enabled. By default, the SYSCON block is clocked by the
    // FRO 12 MHz (fro_12m).
    //
    // TODO: Set up some PLLs here, LPC55 I2C driver has a note about
    // weird crashes with the 150MHz PLL and i2c?

    let syscon = unsafe  { &*device::SYSCON::ptr() };
    let anactrl = unsafe  { &*device::ANACTRL::ptr() };

    // apparently some of the clocks are controlled by the analog block
    //
    anactrl.fro192m_ctrl.modify(|_, w| w.ena_96mhzclk().enable());
    anactrl.fro192m_ctrl.modify(|_, w| w.ena_12mhzclk().enable());


    // Just set our Flexcom0 i.e. UART0 to be 12Mhz
    syscon.fcclksel0().modify( |_, w| w.sel().enum_0x2() );
    // Flexcom4 (the DAC i2c) is also set to 12Mhz
    syscon.fcclksel4().modify( |_, w| w.sel().enum_0x2() );

    // Enable the 1Mhz FRO for utick module
    syscon.clock_ctrl.modify(|_, w| w
        .fro1mhz_clk_ena().enable()
        .fro1mhz_utick_ena().enable()
    );

    // Use the FR0 12MHz clock for the main clock
    syscon.mainclksela.modify(|_, w| w.sel().enum_0x0());
    // Use Main clock A
    syscon.mainclkselb.modify(|_, w| w.sel().enum_0x0());


    unsafe {
        // Divide the AHB clk by 1
        syscon.ahbclkdiv.modify(|_, w| w.div().bits(0x0));
        // 2 system clocks flash access time
        syscon.fmccr.modify(|_, w| w.flashtim().bits(1));
    }

    // Field messages.
    let mask = 0;  // we don't use notifications.
    let mut buffer = 0u32;
    loop {
        let msginfo = sys_recv(buffer.as_bytes_mut(), mask);
        let pmask = 1 << (buffer % 32);
        let chunk = buffer / 32;
        match msginfo.operation {
            // Just like the STM32F4 we end up with a lot of duplication because
            // each register is a different type.
            OP_ENABLE_CLOCK => {
                match chunk {
                    0 => {
                        // Register 0
                        syscon.ahbclkctrl0.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    1 => {
                        // Register 1
                        syscon.ahbclkctrl1.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    2 => {
                        // Register 2
                        syscon.ahbclkctrl2.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                    }
                }
            }
            OP_DISABLE_CLOCK => {
                match chunk {
                    0 => {
                        // Register 0
                        syscon.ahbclkctrl0.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    1 => {
                        // Register 1
                        syscon.ahbclkctrl1.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    2 => {
                        // Register 2
                        syscon.ahbclkctrl2.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                    }
                }
            }
            OP_ENTER_RESET => {
                match chunk {
                    0 => {
                        // Register 0
                        syscon.presetctrl0.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    1 => {
                        // Register 1
                        syscon.presetctrl1.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    2 => {
                        // Register 2
                        syscon.presetctrl1.modify(|r, w| unsafe { w.bits(r.bits() | pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                    }
                }
            }
            OP_LEAVE_RESET => {
                match chunk {
                    0 => {
                        // Register 0
                        syscon.presetctrl0.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    1 => {
                        // Register 1
                        syscon.presetctrl1.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    2 => {
                        // Register 2
                        syscon.presetctrl2.modify(|r, w| unsafe { w.bits(r.bits() & !pmask) });
                        sys_reply(msginfo.sender, ResponseCode::Success as u32, &[]);
                    }
                    _ => {
                        // Huh?
                        sys_reply(msginfo.sender, ResponseCode::BadArg as u32, &[]);
                    }
                }
            }
            _ => {
                // Unrecognized operation code
                sys_reply(msginfo.sender, ResponseCode::BadOp as u32, &[]);
            }
        }
    }
}
