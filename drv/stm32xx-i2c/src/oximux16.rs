// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for a virtual 16-channel mux implemented in an FGPA
//!
//! This is based on the PCA9545, but is controlled by a single 16-bit register
//! (instead of an 8-bit register).

use crate::*;
use bitfield::bitfield;
use drv_i2c_api::{ResponseCode, Segment};

pub struct Oximux16;

bitfield! {
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct ControlRegister(u16);
    channel15_enabled, set_channel15_enabled: 15;
    channel14_enabled, set_channel14_enabled: 14;
    channel13_enabled, set_channel13_enabled: 13;
    channel12_enabled, set_channel12_enabled: 12;
    channel11_enabled, set_channel11_enabled: 11;
    channel10_enabled, set_channel10_enabled: 10;
    channel9_enabled, set_channel9_enabled: 9;
    channel8_enabled, set_channel8_enabled: 8;
    channel7_enabled, set_channel7_enabled: 7;
    channel6_enabled, set_channel6_enabled: 6;
    channel5_enabled, set_channel5_enabled: 5;
    channel4_enabled, set_channel4_enabled: 4;
    channel3_enabled, set_channel3_enabled: 3;
    channel2_enabled, set_channel2_enabled: 2;
    channel1_enabled, set_channel1_enabled: 1;
    channel0_enabled, set_channel0_enabled: 0;
}

impl I2cMuxDriver for Oximux16 {
    fn configure(
        &self,
        mux: &I2cMux<'_>,
        _controller: &I2cController<'_>,
        gpio: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        mux.configure(gpio)
    }

    fn enable_segment(
        &self,
        mux: &I2cMux<'_>,
        controller: &I2cController<'_>,
        segment: Option<Segment>,
    ) -> Result<(), ResponseCode> {
        let mut reg = ControlRegister(0);

        if let Some(segment) = segment {
            match segment {
                Segment::S1 => {
                    reg.set_channel0_enabled(true);
                }
                Segment::S2 => {
                    reg.set_channel1_enabled(true);
                }
                Segment::S3 => {
                    reg.set_channel2_enabled(true);
                }
                Segment::S4 => {
                    reg.set_channel3_enabled(true);
                }
                Segment::S5 => {
                    reg.set_channel4_enabled(true);
                }
                Segment::S6 => {
                    reg.set_channel5_enabled(true);
                }
                Segment::S7 => {
                    reg.set_channel6_enabled(true);
                }
                Segment::S8 => {
                    reg.set_channel7_enabled(true);
                }
                Segment::S9 => {
                    reg.set_channel8_enabled(true);
                }
                Segment::S10 => {
                    reg.set_channel9_enabled(true);
                }
                Segment::S11 => {
                    reg.set_channel10_enabled(true);
                }
                Segment::S12 => {
                    reg.set_channel11_enabled(true);
                }
                Segment::S13 => {
                    reg.set_channel12_enabled(true);
                }
                Segment::S14 => {
                    reg.set_channel13_enabled(true);
                }
                Segment::S15 => {
                    reg.set_channel14_enabled(true);
                }
                Segment::S16 => {
                    reg.set_channel15_enabled(true);
                }
            }
        }

        //
        // This part has but one register -- any write is to the control
        // register.
        //
        match controller.write_read(
            mux.address,
            2,
            |i| Some(reg.0.to_le_bytes()[i]),
            ReadLength::Fixed(0),
            |_, _| Some(()),
        ) {
            Err(code) => Err(mux.error_code(code)),
            _ => {
                // The mux takes ~100µs to electrically switch over; we could
                // optimize this later by using a dedicated timer with a shorter
                // delay.
                userlib::hl::sleep_for(1);
                Ok(())
            }
        }
    }

    fn reset(
        &self,
        mux: &I2cMux<'_>,
        gpio: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        mux.reset(gpio)
    }
}
