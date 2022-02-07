// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the PCA9548 I2C mux

use crate::*;
use bitfield::bitfield;
use drv_i2c_api::{ResponseCode, Segment};

pub struct Pca9548;

bitfield! {
    #[derive(Copy, Clone, PartialEq)]
    pub struct ControlRegister(u8);
    channel7_enabled, set_channel7_enabled: 7;
    channel6_enabled, set_channel6_enabled: 6;
    channel5_enabled, set_channel5_enabled: 5;
    channel4_enabled, set_channel4_enabled: 4;
    channel3_enabled, set_channel3_enabled: 3;
    channel2_enabled, set_channel2_enabled: 2;
    channel1_enabled, set_channel1_enabled: 1;
    channel0_enabled, set_channel0_enabled: 0;
}

impl I2cMuxDriver for Pca9548 {
    fn configure(
        &self,
        mux: &I2cMux,
        _controller: &I2cController,
        gpio: &sys_api::Sys,
        _ctrl: &I2cControl,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        mux.configure(gpio)
    }

    fn enable_segment(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        segment: Segment,
        ctrl: &I2cControl,
    ) -> Result<(), ResponseCode> {
        let mut reg = ControlRegister(0);

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
        }

        //
        // This part has but one register -- any write is to the control
        // register.
        //
        match controller.write_read(
            mux.address,
            1,
            |_| Some(reg.0),
            ReadLength::Fixed(0),
            |_, _| Some(()),
            ctrl,
        ) {
            Err(code) => Err(mux.error_code(code)),
            _ => Ok(()),
        }
    }

    fn reset(
        &self,
        mux: &I2cMux,
        gpio: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        mux.reset(gpio)
    }
}
