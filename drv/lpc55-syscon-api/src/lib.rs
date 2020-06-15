//! Client API for the LPC55S6x SYSCON block
//!
//! This driver is responsible for clocks (peripherals and PLLs), systick
//! callibration, memory remapping, id registers. Most drivers will be
//! interested in the clock bits.
//!
//! # Peripheral numbering
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

#![no_std]

use zerocopy::AsBytes;

use userlib::*;

#[derive(FromPrimitive)]
enum Op {
    EnableClock = 1,
    DisableClock = 2,
    EnterReset = 3,
    LeaveReset = 4,
}

#[derive(Clone, Debug)]
pub struct Syscon(TaskId);

impl From<TaskId> for Syscon {
    fn from(t: TaskId) -> Self {
        Self(t)
    }
}

impl Syscon {
    /// Requests that the clock to a peripheral be turned on.
    ///
    /// Peripherals are numbered by bit number in the SYSCON registers
    ///
    /// - `PRESETCTRL0[31:0]` are indices 31-0.
    /// - `PRESETCTRL1[31:0]` are indices 63-32.
    /// - `PRESETCTRL2[31:0]` are indices 64-96.
    ///
    /// # Panics
    ///
    /// If you provide an out-of-range peripheral number, or if the syscon
    /// server has died.
    pub fn enable_clock(&self, number: u32) {
        #[derive(AsBytes)]
        #[repr(C)]
        struct EnableClock(u32);

        impl hl::Call for EnableClock {
            const OP: u16 = Op::EnableClock as u16;
            type Response = ();
            type Err = u32;
        }

        hl::send(self.0, &EnableClock(number)).unwrap()
    }

    /// Requests that the clock to a peripheral be turned off.
    ///
    /// Peripherals are numbered by bit number in the SYSCON registers
    ///
    /// - `PRESETCTRL0[31:0]` are indices 31-0.
    /// - `PRESETCTRL1[31:0]` are indices 63-32.
    /// - `PRESETCTRL2[31:0]` are indices 64-96.
    ///
    /// # Panics
    ///
    /// If you provide an out-of-range peripheral number, or if the syscon
    /// server has died.
    pub fn disable_clock(&self, number: u32) {
        #[derive(AsBytes)]
        #[repr(C)]
        struct DisableClock(u32);

        impl hl::Call for DisableClock {
            const OP: u16 = Op::DisableClock as u16;
            type Response = ();
            type Err = u32;
        }

        hl::send(self.0, &DisableClock(number)).unwrap()
    }

    /// Requests that the reset line to a peripheral be asserted.
    ///
    /// Peripherals are numbered by bit number in the SYSCON registers
    ///
    /// - `PRESETCTRL0[31:0]` are indices 31-0.
    /// - `PRESETCTRL1[31:0]` are indices 63-32.
    /// - `PRESETCTRL2[31:0]` are indices 64-96.
    ///
    /// # Panics
    ///
    /// If you provide an out-of-range peripheral number, or if the syscon
    /// server has died.
    pub fn enter_reset(&self, number: u32) {
        #[derive(AsBytes)]
        #[repr(C)]
        struct EnterReset(u32);

        impl hl::Call for EnterReset {
            const OP: u16 = Op::EnterReset as u16;
            type Response = ();
            type Err = u32;
        }

        hl::send(self.0, &EnterReset(number)).unwrap()
    }

    /// Requests that the reset line to a peripheral be deasserted.
    ///
    /// Peripherals are numbered by bit number in the SYSCON registers
    ///
    /// - `PRESETCTRL0[31:0]` are indices 31-0.
    /// - `PRESETCTRL1[31:0]` are indices 63-32.
    /// - `PRESETCTRL2[31:0]` are indices 64-96.
    ///
    /// # Panics
    ///
    /// If you provide an out-of-range peripheral number, or if the syscon
    /// server has died.
    pub fn leave_reset(&self, number: u32) {
        #[derive(AsBytes)]
        #[repr(C)]
        struct LeaveReset(u32);

        impl hl::Call for LeaveReset {
            const OP: u16 = Op::LeaveReset as u16;
            type Response = ();
            type Err = u32;
        }

        hl::send(self.0, &LeaveReset(number)).unwrap()
    }
}
