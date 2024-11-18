// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
//
// For the STM32H7xx, see ARMv7-M Architecture Reference Manual
// Part 3 Debug Arch.
// https://developer.arm.com/documentation/ddi0403/d/Debug-Architecture?lang=en

use bitflags::bitflags;
use userlib::FromPrimitive;

pub trait DpAddressable {
    /// Address for accessing a DP Register
    const ADDRESS: u32;
}

// For keeping track of unwinding debug actions
bitflags! {
    #[derive(PartialEq, Eq, Copy, Clone)]
    pub struct Undo: u8 {
        // Need self.swd_finish()
        const SWD = 1 << 0;
        // Need self.sp_reset_leave(true)
        const RESET = 1 << 1;
        // Need DEMCR = 0
        const VC_CORERESET = 1 << 2;
        // Need to clear debug enable.
        const DEBUGEN = 1 << 3;
    }
}

// RW   0x00000000    Debug Halting Control and Status Register
// Some DHCSR bits have different read vs. write meanings
// Specifically, the MAGIC value enables writing other control bits
// but some of those bits are status bits when read.
bitflags! {
    #[derive(PartialEq, Eq, Copy, Clone)]
    pub struct Dhcsr: u32 {
        // At least one reset since last DHCSR read. clear on read.
        const S_RESET_ST = 1 << 25;
        const S_RETIRE_ST = 1 << 24;
        const S_LOCKUP = 1 << 19;
        const S_SLEEP = 1 << 18;
        const S_HALT = 1 << 17;
        const S_REGRDY = 1 << 16;

        // Magic number allows setting C_* bits.
        const DBGKEY = 0xA05F << 16;

        const C_SNAPSTALL = 1 << 5;
        const C_MASKINTS = 1 << 3;
        const C_STEP = 1 << 2;
        const C_HALT = 1 << 1;
        const C_DEBUGEN = 1 << 0;
        const _ = !0;
    }
}

impl From<u32> for Dhcsr {
    fn from(v: u32) -> Self {
        Self::from_bits_retain(v)
    }
}

impl DpAddressable for Dhcsr {
    const ADDRESS: u32 = 0xE000EDF0;
}

impl Dhcsr {
    pub fn halt() -> Self {
        Self::DBGKEY | Self::C_HALT | Self::C_DEBUGEN
    }
    /// Clear C_HALT while keeping debug control.
    pub fn resume() -> Self {
        Self::DBGKEY | Self::C_DEBUGEN
    }
    pub fn end_debug() -> Self {
        Self::DBGKEY
    }
    pub fn is_halted(self) -> bool {
        self & Self::S_HALT == Self::S_HALT
    }
    pub fn is_regrdy(self) -> bool {
        self & Self::S_REGRDY == Self::S_REGRDY
    }
    // Just to document the remaining bit:
    // pub fn is_lockup(self) -> bool {
    //     self & Self::S_LOCKUP == Self::S_LOCKUP
    // }
}

// Debug Core Register Selector Register
pub const DCRSR: u32 = 0xE000EDF4;
// Debug Core Register Data Register
pub const DCRDR: u32 = 0xE000EDF8;

// DEMCR RW   0x00000000    Debug Exception and Monitor Control Register
bitflags! {
    #[derive(PartialEq, Eq, Copy, Clone)]
    pub struct Demcr: u32 {
        const MON_EN = 1 << 16;
        const VC_HARDERR = 1 << 10;
        const VC_INTERR = 1 << 9;
        const VC_BUSERR = 1 << 8;
        const VC_STATERR = 1 << 7;
        const VC_CHKERR = 1 << 6;
        const VC_NOCPERR = 1 << 5;
        const VC_MMERR = 1 << 4;
        const VC_CORERESET = 1 << 0;
    }
}

impl DpAddressable for Demcr {
    const ADDRESS: u32 = 0xE000EDFC;
}

// Armv7-M Arch. Ref. Manual - C1.6.1 Debug Fault Status Register
// RW   Init: 0x00000000[on power-on reset only]
bitflags! {
    #[derive(PartialEq, Eq, Copy, Clone)]
    pub struct Dfsr: u32 {
        // Assertion of an external debug request
        const EXTERNAL = 1 << 4;
        // Vector catch triggered
        const VCATCH = 1 << 3;
        // At least one DWT event
        const DWTTRAP = 1 << 2;
        // Breakpoint
        const BKPT = 1 << 1;
        // Halt request debug event.
        // • A C_HALT or C_STEP request, triggered by a write to the DHCSR,
        //   see Debug Halting Control and Status Register, DHCSR.
        // • A step request triggered by setting DEMCR.MON_STEP to 1,
        //   see Debug monitor stepping on page C1-696.
        const HALTED = 1 << 0;
        const _ = !0;
    }
}

impl DpAddressable for Dfsr {
    const ADDRESS: u32 = 0xE000ED30;
}

impl Dfsr {
    pub fn _is_faulted(self) -> bool {
        (self
            & (Self::EXTERNAL
                | Self::VCATCH
                | Self::DWTTRAP
                | Self::BKPT
                | Self::HALTED))
            .bits()
            != 0
    }
    pub fn is_vcatch(self) -> bool {
        self & Self::VCATCH == Self::VCATCH
    }
}

// See Armv7-M Architecture Reference Manual - C1.6.3 - REGSEL
#[derive(PartialEq, Copy, Clone, FromPrimitive)]
#[repr(u16)]
pub enum Reg {
    R0 = 0b0000000,
    R1 = 0b0000001,
    R2 = 0b0000010,
    R3 = 0b0000011,
    R4 = 0b0000100,
    R5 = 0b0000101,
    R6 = 0b0000110,
    R7 = 0b0000111,
    R8 = 0b0001000,
    R9 = 0b0001001,
    R10 = 0b0001010,
    R11 = 0b0001011,
    R12 = 0b0001100,
    Sp = 0b0001101,
    Lr = 0b0001110,
    Dr = 0b0001111, // DebugReturnAddress, see C1-704
    Xpsr = 0b0010000,
    Msp = 0b0010001,   // Main stack pointer
    Psp = 0b0010010,   // Process stack pointer
    Cfbp = 0b0010100, // [31:24] CONTROL, [23:15] FAULTMASK, [15:8] BASEPRI, [7:0] PRIMASK
    Fpscr = 0b0100001, // Floating Point Status and Control Register
    S0 = 0b1000000,
    S1 = 0b1000001,
    S2 = 0b1000010,
    S3 = 0b1000011,
    S4 = 0b1000100,
    S5 = 0b1000101,
    S6 = 0b1000110,
    S7 = 0b1000111,
    S8 = 0b1001000,
    S9 = 0b1001001,
    S10 = 0b1001010,
    S11 = 0b1001011,
    S12 = 0b1001100,
    S13 = 0b1001101,
    S14 = 0b1001110,
    S15 = 0b1001111,
    S16 = 0b1010000,
    S17 = 0b1010001,
    S18 = 0b1010010,
    S19 = 0b1010011,
    S20 = 0b1010100,
    S21 = 0b1010101,
    S22 = 0b1010110,
    S23 = 0b1010111,
    S24 = 0b1011000,
    S25 = 0b1011001,
    S26 = 0b1011010,
    S27 = 0b1011011,
    S28 = 0b1011100,
    S29 = 0b1011101,
    S30 = 0b1011110,
    S31 = 0b1011111,
}

pub const _ICSR: u32 = 0xE000ED04;
pub const VTOR: u32 = 0xE000ED08;
