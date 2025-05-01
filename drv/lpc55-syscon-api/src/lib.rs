// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the LPC55S6x SYSCON block
//!
//! This driver is responsible for clocks (peripherals and PLLs), systick
//! callibration, memory remapping, id registers. Most drivers will be
//! interested in the clock bits.

#![no_std]

use userlib::*;
use zerocopy::{Immutable, IntoBytes, KnownLayout};

/// Peripheral numbering.
///
/// Peripheral bit numbers per the LPC55 manual section 4.5 (for the benefit of
/// the author writing this driver who hates having to look these up. Double
/// check these later!)
///
/// Peripherals are numbered by bit number in the SYSCON registers
///
/// - `PRESETCTRL0[31:0]` are indices 31-0.
/// - `PRESETCTRL1[31:0]` are indices 63-32.
/// - `PRESETCTRL2[31:0]` are indices 64-96.
#[derive(
    Copy,
    Clone,
    Eq,
    PartialEq,
    Debug,
    FromPrimitive,
    Immutable,
    KnownLayout,
    IntoBytes,
)]
#[repr(u32)]
pub enum Peripheral {
    Rom = 1,
    SramCtrl1 = 3,
    SramCtrl2 = 4,
    SramCtrl3 = 5,
    SramCtrl4 = 6,
    Flash = 7,
    Fmc = 8,
    Mux = 11,
    Iocon = 13,
    Gpio0 = 14,
    Gpio1 = 15,
    Pint = 18,
    Gint = 19,
    Dma0 = 20,
    Crcgen = 21,
    Wwdt = 22,
    Rtc = 23,
    Mailbox = 26,
    Adc = 27,
    Mrt = 32 + 0,
    Ostimer = 32 + 1,
    Sct = 32 + 2,
    Utick = 32 + 10,
    Fc0 = 32 + 11,
    Fc1 = 32 + 12,
    Fc2 = 32 + 13,
    Fc3 = 32 + 14,
    Fc4 = 32 + 15,
    Fc5 = 32 + 16,
    Fc6 = 32 + 17,
    Fc7 = 32 + 18,
    Timer2 = 32 + 22,
    Usb0Dev = 32 + 25,
    Timer0 = 32 + 26,
    Timer1 = 32 + 27,
    Dma1 = 32 + 32 + 1,
    Comp = 32 + 32 + 2,
    Sdio = 32 + 32 + 3,
    Usb1Host = 32 + 32 + 4,
    Usb1Dev = 32 + 32 + 5,
    Usb1Ram = 32 + 32 + 6,
    Usb1Phy = 32 + 32 + 7,
    Freqme = 32 + 32 + 8,
    Rng = 32 + 32 + 13,
    Sysctl = 32 + 32 + 15,
    Usb0Hostm = 32 + 32 + 16,
    Usb0Hosts = 32 + 32 + 17,
    HashAes = 32 + 32 + 18,
    Pq = 32 + 32 + 19,
    Plulut = 32 + 32 + 20,
    Timer3 = 32 + 32 + 21,
    Timer4 = 32 + 32 + 22,
    Puf = 32 + 32 + 23,
    Casper = 32 + 32 + 24,
    AnalogCtrl = 32 + 32 + 27,
    HsLspi = 32 + 32 + 28,
    GpioSec = 32 + 32 + 29,
    GpioSecInt = 32 + 32 + 30,
}

pub enum Reg {
    R0 = 0,
    R1 = 1,
    R2 = 2,
}

impl Peripheral {
    pub fn reg_num(&self) -> Reg {
        match (*self as usize) / 32 {
            0 => Reg::R0,
            1 => Reg::R1,
            2 => Reg::R2,
            _ => panic!(),
        }
    }

    pub fn pmask(&self) -> u32 {
        1 << ((*self as u32) % 32)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
