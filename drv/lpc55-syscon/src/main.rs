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
use cortex_m;

use userlib::*;

#[derive(FromPrimitive)]
enum Op {
    EnableClock = 1,
    DisableClock = 2,
    EnterReset = 3,
    LeaveReset = 4,
}

#[derive(FromPrimitive)]
enum Reg {
    R0 = 0,
    R1 = 1,
    R2 = 2,
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

macro_rules! set_bit {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() | $mask) });
    };
}

macro_rules! clear_bit {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() & !$mask) });
    };
}

#[export_name = "main"]
fn main() -> ! {
    // From the manual:
    //
    // The clock to the SYSCON block is
    // always enabled. By default, the SYSCON block is clocked by the
    // FRO 12 MHz (fro_12m).
    //
    // LPC55 I2C driver has a note about weird crashes with the 150MHz PLL
    // and i2c?

    let syscon = unsafe  { &*device::SYSCON::ptr() };
    let anactrl = unsafe  { &*device::ANACTRL::ptr() };
    let pmc = unsafe  { &*device::PMC::ptr() };

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

    // Use the FR0 12MHz clock for the main clock to start
    // We'll be switching over to the PLL later
    syscon.mainclksela.modify(|_, w| w.sel().enum_0x0());
    // Use Main clock A
    syscon.mainclkselb.modify(|_, w| w.sel().enum_0x0());

    // Divide the AHB clk by 1
    syscon.ahbclkdiv.modify(|_, w| unsafe { w.div().bits(0x0) });
    // 2 system clocks flash access time
    syscon.fmccr.modify(|_, w| unsafe { w.flashtim().bits(1) });


    // Some PLL math: Per 4.6.6.3.1 in the manual:
    //
    //  F_out = F_cco / (2 * P) = F_in * M / (N * 2 * P)
    //
    //  or written out in a nicer way
    //
    //  F_out * N * P * 2 = M * F_in
    //
    //  With a note that F_cco has to be > 275 Mhz. We want
    //  the maximum 150 Mhz. There are multiple solutions to
    //  this equation but the ones which seem to be stable are
    //  F_in = 12 Mhz
    //  N = 8
    //  P = 1
    //  M = 200
    //
    let pll_n = 8;
    let pll_p = 1;
    let pll_m = 200;

    // From 4.6.6.3.2 we need to calculate the bandwidth
    //
    // selp = floor(M/4) + 1
    //
    // selp = floor(200/4) + 1
    //
    // selp = 50 + 1 which gets rounded down to 31
    //
    // if (M >= 8000) => seli = 1
    // if (8000 > M >= 122) => seli = floor(8000/M)
    // if (122 > M >= 1) => seli = 2 * floor(M/4) + 3
    //
    // seli = floor(8000/M)
    //
    // seli = 40
    //
    // For normal applications the value for selr[3:0] must be kept 0.
    //
    let selp = 31;
    let seli = 40;
    let selr = 0;

    // Make sure these are actually off
    pmc.pdruncfg0.modify(|_, w| w
        .pden_pll0().poweredoff()
        .pden_pll0_sscg().poweredoff()
    );

    // Mark PLL0 as using 12 MHz
    syscon.pll0clksel.modify(|_, w| w.sel().enum_0x0());

    syscon.pll0ctrl.modify(|_, w| unsafe { w
                .selr().bits(selr)
                .seli().bits(seli)
                .selp().bits(selp)
                .clken().enable() });

    // writing these settings is 'quirky'. We have to write the
    // value once into the register then write it again with the latch
    // bit set. Not in the docs but in the NXP C driver...
    syscon.pll0ndec.write(|w| unsafe { w
            .ndiv().bits(pll_n)
    });
    syscon.pll0ndec.write(|w| unsafe { w
            .ndiv().bits(pll_n)
            .nreq().set_bit()
    });

    syscon.pll0pdec.write(|w| unsafe { w
            .pdiv().bits(pll_p)
    });
    syscon.pll0pdec.write(|w| unsafe { w
            .pdiv().bits(pll_p)
            .preq().set_bit()
    });

    syscon.pll0sscg1.write(|w| unsafe { w
            .mdiv_ext().bits(pll_m)
            .sel_ext().set_bit()
    });
    syscon.pll0sscg1.write(|w| unsafe { w
            .mdiv_ext().bits(pll_m)
            .sel_ext().set_bit()
            .mreq().set_bit()
            .md_req().set_bit()
    });

    // Now actually turn on the PLLs
    pmc.pdruncfg0.modify(|_, w| w
        .pden_pll0().poweredon()
        .pden_pll0_sscg().poweredon()
    );

    // Time to put the Lock in Phase Locked Loop!
    //
    // 4.6.6.5.2 The start-up time is 500 μs + 300 / Fref seconds
    // The NXP C driver (i.e. documentation) just uses 6 ms for everything
    // even if we need a significantly lower start up time with Fref
    // of 12 mhz based on that formula. We can check this out later.
    //
    // Now of course comes the question of how we delay. The
    // cortex-m crate has a delay function to delay for at least
    // n instruction cycles. We're running at a maximum 150 mhz.
    //
    // n ins = 6 msec * 150 000 000 ins / sec * 1 sec / 1 000 msec
    //
    //  = 900000 instructions
    //
    // ...which is certainly an upper bound that can be adjusted
    //
    cortex_m::asm::delay(900000);

    // The flash wait cycles need to be adjusted. Per the docs
    // 0xb = 12 system clocks flash access time (for system clock rates up
    // to 150 MHz).
    syscon.fmccr.modify(|_, w| unsafe { w.flashtim().bits(0xb) });

    // Now actually set our clocks
    // Main A = 12 MHz
    syscon.mainclksela.modify(|_, w| w.sel().enum_0x0());
    // Main B = PLL0
    syscon.mainclkselb.modify(|_, w| w.sel().enum_0x1());


    // Field messages.
    // Ensure our buffer is aligned properly for a u32 by declaring it as one.
    let mut buffer = [0u32; 1];
    loop {
        hl::recv_without_notification(
            buffer.as_bytes_mut(),
            |op, msg| -> Result<(), ResponseCode> {
                // Every incoming message uses the same payload type and
                // response type: it's always u32 -> (). So we can do the
                // check-and-convert here:
                let (msg, caller) = msg.fixed::<u32, ()>()
                    .ok_or(ResponseCode::BadArg)?;
                let pmask = 1 << (msg % 32);
                let chunk = msg / 32;

                let reg = Reg::from_u32(chunk)
                    .ok_or(ResponseCode::BadArg)?;

                // Just like the STM32F4 we end up with a lot of duplication
                // because each register is a different type.
                match op {
                    Op::EnableClock => match reg {
                        Reg::R0 => set_bit!(syscon.ahbclkctrl0, pmask),
                        Reg::R1 => set_bit!(syscon.ahbclkctrl1, pmask),
                        Reg::R2 => set_bit!(syscon.ahbclkctrl2, pmask),
                    }
                    Op::DisableClock => match reg {
                        Reg::R0 => clear_bit!(syscon.ahbclkctrl0, pmask),
                        Reg::R1 => clear_bit!(syscon.ahbclkctrl1, pmask),
                        Reg::R2 => clear_bit!(syscon.ahbclkctrl2, pmask),
                    }
                    Op::EnterReset => match reg {
                        Reg::R0 => set_bit!(syscon.presetctrl0, pmask),
                        Reg::R1 => set_bit!(syscon.presetctrl1, pmask),
                        Reg::R2 => set_bit!(syscon.presetctrl2, pmask),
                    }
                    Op::LeaveReset => match reg {
                        Reg::R0 => clear_bit!(syscon.presetctrl0, pmask),
                        Reg::R1 => clear_bit!(syscon.presetctrl1, pmask),
                        Reg::R2 => clear_bit!(syscon.presetctrl2, pmask),
                    }
                }

                caller.reply(());
                Ok(())
            }
        );
    }
}
