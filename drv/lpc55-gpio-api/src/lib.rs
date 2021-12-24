// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

use core::cell::Cell;
use userlib::*;
use zerocopy::{AsBytes, FromBytes};

// Only the expresso boards have the full 64 pins, the
// LPC55S2x variant only has 36
//
// These are encoded so port 0 goes from 0 - 31 and port 1
// goes 0 - 64
cfg_if::cfg_if! {
    if #[cfg(any(target_board = "lpcxpresso55s69"))] {
        #[derive(Copy, Clone, Debug, FromPrimitive)]
        #[repr(u32)]
        pub enum Pin {
            PIO0_0 = 0,
            PIO0_1 = 1,
            PIO0_2 = 2,
            PIO0_3 = 3,
            PIO0_4 = 4,
            PIO0_5 = 5,
            PIO0_6 = 6,
            PIO0_7 = 7,
            PIO0_8 = 8,
            PIO0_9 = 9,
            PIO0_10 = 10,
            PIO0_11 = 11,
            PIO0_12 = 12,
            PIO0_13 = 13,
            PIO0_14 = 14,
            PIO0_15 = 15,
            PIO0_16 = 16,
            PIO0_17 = 17,
            PIO0_18 = 18,
            PIO0_19 = 19,
            PIO0_20 = 20,
            PIO0_21 = 21,
            PIO0_22 = 22,
            PIO0_23 = 23,
            PIO0_24 = 24,
            PIO0_25 = 25,
            PIO0_26 = 26,
            PIO0_27 = 27,
            PIO0_28 = 28,
            PIO0_29 = 29,
            PIO0_30 = 30,
            PIO0_31 = 31,

            PIO1_0 = 32,
            PIO1_1 = 33,
            PIO1_2 = 34,
            PIO1_3 = 35,
            PIO1_4 = 36,
            PIO1_5 = 37,
            PIO1_6 = 38,
            PIO1_7 = 39,
            PIO1_8 = 40,
            PIO1_9 = 41,
            PIO1_10 = 42,
            PIO1_11 = 43,
            PIO1_12 = 44,
            PIO1_13 = 45,
            PIO1_14 = 46,
            PIO1_15 = 47,
            PIO1_16 = 48,
            PIO1_17 = 49,
            PIO1_18 = 50,
            PIO1_19 = 51,
            PIO1_20 = 52,
            PIO1_21 = 53,
            PIO1_22 = 54,
            PIO1_23 = 55,
            PIO1_24 = 56,
            PIO1_25 = 57,
            PIO1_26 = 58,
            PIO1_27 = 59,
            PIO1_28 = 60,
            PIO1_29 = 61,
            PIO1_30 = 62,
            PIO1_31 = 63,
        }

    } else {
        #[derive(Copy, Clone, Debug, FromPrimitive)]
        #[repr(u32)]
        pub enum Pin {
            PIO0_0 = 0,
            PIO0_1 = 1,
            PIO0_2 = 2,
            PIO0_3 = 3,
            PIO0_4 = 4,
            PIO0_5 = 5,
            PIO0_6 = 6,
            PIO0_7 = 7,
            PIO0_8 = 8,
            PIO0_9 = 9,
            PIO0_10 = 10,
            PIO0_11 = 11,
            PIO0_12 = 12,
            PIO0_13 = 13,
            PIO0_14 = 14,
            PIO0_15 = 15,
            PIO0_16 = 16,
            PIO0_17 = 17,
            PIO0_18 = 18,
            PIO0_19 = 19,
            PIO0_20 = 20,
            PIO0_21 = 21,
            PIO0_22 = 22,
            PIO0_23 = 23,
            PIO0_24 = 24,
            PIO0_25 = 25,
            PIO0_26 = 26,
            PIO0_27 = 27,
            PIO0_28 = 28,
            PIO0_29 = 29,
            PIO0_30 = 30,
            PIO0_31 = 31,

            PIO1_0 = 32,
            PIO1_1 = 33,
            PIO1_2 = 34,
            PIO1_3 = 35,
        }

    }

}
#[derive(Copy, Clone, Debug, FromPrimitive)]
pub enum Mode {
    NoPull = 0,
    PullDown = 1,
    PullUp = 2,
    Repeater = 3,
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum Digimode {
    Analog = 0,
    Digital = 1,
}

impl Into<bool> for Digimode {
    fn into(self) -> bool {
        self == Self::Digital
    }
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum Slew {
    Standard = 0,
    Fast = 1,
}

impl Into<bool> for Slew {
    fn into(self) -> bool {
        self == Self::Fast
    }
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum Invert {
    Disable = 0,
    Enabled = 1,
}

impl Into<bool> for Invert {
    fn into(self) -> bool {
        self == Self::Enabled
    }
}

#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
pub enum Opendrain {
    Normal = 0,
    Opendrain = 1,
}

impl Into<bool> for Opendrain {
    fn into(self) -> bool {
        self == Self::Opendrain
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
pub enum Op {
    SetDir = 1,
    SetVal = 2,
    ReadVal = 3,
    Configure = 4,
    Toggle = 5,
}

#[derive(Copy, Clone, Debug, FromPrimitive)]
pub enum AltFn {
    // GPIO functionality is always Alt0
    Alt0 = 0,
    Alt1 = 1,
    Alt2 = 2,
    Alt3 = 3,
    Alt4 = 4,
    Alt5 = 5,
    Alt6 = 6,
    Alt7 = 7,
    Alt8 = 8,
    Alt9 = 9,
}

#[derive(Debug, FromPrimitive)]
pub enum Direction {
    Input = 0,
    Output = 1,
}

#[derive(Debug, FromPrimitive)]
pub enum Value {
    Zero = 0,
    One = 1,
}

#[derive(Clone, Debug)]
pub struct Gpio(Cell<TaskId>);

impl From<TaskId> for Gpio {
    fn from(t: TaskId) -> Self {
        Self(Cell::new(t))
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(u32)]
pub enum GpioError {
    BadArg = 2,
}

impl From<u32> for GpioError {
    fn from(x: u32) -> Self {
        match x {
            2 => GpioError::BadArg,
            _ => panic!(),
        }
    }
}

impl From<GpioError> for u32 {
    fn from(rc: GpioError) -> Self {
        rc as u32
    }
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct ConfigureRequest {
    pub pin: u32,
    pub conf: u32,
}

impl hl::Call for ConfigureRequest {
    const OP: u16 = Op::Configure as u16;
    type Response = ();
    type Err = GpioError;
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct DirectionRequest {
    pub pin: u32,
    pub dir: u32,
}

impl hl::Call for DirectionRequest {
    const OP: u16 = Op::SetDir as u16;
    type Response = ();
    type Err = GpioError;
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct SetRequest {
    pub pin: u32,
    pub val: u32,
}

impl hl::Call for SetRequest {
    const OP: u16 = Op::SetVal as u16;
    type Response = ();
    type Err = GpioError;
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct ReadRequest {
    pub pin: u32,
}

impl hl::Call for ReadRequest {
    const OP: u16 = Op::ReadVal as u16;
    type Response = u8;
    type Err = GpioError;
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct ToggleRequest {
    pub pin: u32,
}

impl hl::Call for ToggleRequest {
    const OP: u16 = Op::Toggle as u16;
    type Response = ();
    type Err = GpioError;
}

impl Gpio {
    pub fn iocon_configure(
        &self,
        pin: Pin,
        alt: AltFn,
        mode: Mode,
        slew: Slew,
        invert: Invert,
        digimode: Digimode,
        od: Opendrain,
    ) -> Result<(), GpioError> {
        // This is the format specified by the LPC55 manual. Trying to pass
        // each of the enums individually would get expensive space wise!
        let conf = (alt as u32)
            | (mode as u32) << 4
            | (slew as u32) << 6
            | (invert as u32) << 7
            | (digimode as u32) << 8
            | (od as u32) << 9;

        hl::send_with_retry(
            &self.0,
            &ConfigureRequest {
                pin: pin as u32,
                conf,
            },
        )
    }

    // Direction is treated as a property of GPIO as opposed to IOCON which
    // deals with the raw pins. It's a bit of a odd split but we only use
    // direction in a few actual places in the code
    pub fn set_dir(
        &self,
        pin: Pin,
        direction: Direction,
    ) -> Result<(), GpioError> {
        hl::send_with_retry(
            &self.0,
            &DirectionRequest {
                pin: pin as u32,
                dir: direction as u32,
            },
        )
    }

    pub fn set_val(&self, pin: Pin, val: Value) -> Result<(), GpioError> {
        hl::send_with_retry(
            &self.0,
            &SetRequest {
                pin: pin as u32,
                val: val as u32,
            },
        )
    }

    pub fn read_val(&self, pin: Pin) -> Result<Value, GpioError> {
        let result =
            hl::send_with_retry(&self.0, &ReadRequest { pin: pin as u32 })?;

        Value::from_u8(result).ok_or(GpioError::BadArg)
    }

    pub fn toggle(&self, pin: Pin) -> Result<(), GpioError> {
        hl::send_with_retry(&self.0, &ToggleRequest { pin: pin as u32 })
    }
}
