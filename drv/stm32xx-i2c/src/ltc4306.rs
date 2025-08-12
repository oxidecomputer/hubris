// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the LTC4306 I2C mux

use crate::*;
use bitfield::bitfield;
use drv_i2c_api::{ResponseCode, Segment};
use ringbuf::*;
use userlib::*;

pub struct Ltc4306;

bitfield! {
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct Register0(u8);
    connected, _: 7;
    not_alert1, _: 6;
    not_alert2, _: 5;
    not_alert3, _: 4;
    not_alert4, _: 3;
    not_failed, _: 2;
    latched_timeout, _: 1;
    timeout, _: 0;
}

bitfield! {
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct Register1(u8);
    upstream_accelerators_enable, set_upstream_accelerators_enable: 7;
    downstream_accelerators_enable, set_downstream_accelerators_enable: 6;
    gpio1_output_state, set_gpio1_output_state: 5;
    gpio2_output_state, set_gpio2_output_state: 4;
    gpio1_logic_state, _: 1;
    gpio2_logic_state, _: 0;
}

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
#[allow(clippy::enum_variant_names)] // not great, TODO
enum Timeout {
    TimeoutDisabled = 0b00,
    Timeout30ms = 0b01,
    Timeout15ms = 0b10,
    Timeout7point5ms = 0b11,
}

impl From<u8> for Timeout {
    fn from(value: u8) -> Self {
        Timeout::from_u8(value).unwrap()
    }
}

impl From<Timeout> for u8 {
    fn from(value: Timeout) -> Self {
        value as u8
    }
}

bitfield! {
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct Register2(u8);
    gpio1_mode_input, set_gpio1_mode_input: 7;
    gpio2_mode_input, set_gpio2_mode_input: 6;
    connect_regardless, set_connect_regardless: 5;
    gpio1_push_pull, set_gpio1_push_pull: 4;
    gpio2_push_pull, set_gpio2_push_pull: 3;
    mass_write_enabled, set_mass_write_enabled: 2;
    from into Timeout, timeout, set_timeout: 1, 0;
}

bitfield! {
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct Register3(u8);
    bus1_connected, set_bus1_connected: 7;
    bus2_connected, set_bus2_connected: 6;
    bus3_connected, set_bus3_connected: 5;
    bus4_connected, set_bus4_connected: 4;
    bus1_active, _: 3;
    bus2_active, _: 2;
    bus3_active, _: 1;
    bus4_active, _: 0;
}

ringbuf!(u8, 16, 0);

fn read_reg_u8(
    mux: &I2cMux<'_>,
    controller: &I2cController<'_>,
    reg: u8,
) -> Result<u8, ResponseCode> {
    let mut rval = 0u8;
    let wlen = 1;

    let controller_result = controller.write_read(
        mux.address,
        wlen,
        |_| Some(reg),
        ReadLength::Fixed(1),
        |_, byte| {
            rval = byte;
            Some(())
        },
    );
    match controller_result {
        Err(code) => Err(mux.error_code(code)),
        _ => Ok(rval),
    }
}

fn write_reg_u8(
    mux: &I2cMux<'_>,
    controller: &I2cController<'_>,
    reg: u8,
    val: u8,
) -> Result<(), ResponseCode> {
    match controller.write_read(
        mux.address,
        2,
        |pos| Some(if pos == 0 { reg } else { val }),
        ReadLength::Fixed(0),
        |_, _| Some(()),
    ) {
        Err(code) => Err(mux.error_code(code)),
        _ => Ok(()),
    }
}

impl I2cMuxDriver for Ltc4306 {
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
        let mut reg3 = Register3(0);

        if let Some(segment) = segment {
            match segment {
                Segment::S1 => {
                    reg3.set_bus1_connected(true);
                }
                Segment::S2 => {
                    reg3.set_bus2_connected(true);
                }
                Segment::S3 => {
                    reg3.set_bus3_connected(true);
                }
                Segment::S4 => {
                    reg3.set_bus4_connected(true);
                }
                _ => {
                    return Err(ResponseCode::SegmentNotFound);
                }
            }
        }

        write_reg_u8(mux, controller, 3, reg3.0)?;
        let reg0 = Register0(read_reg_u8(mux, controller, 0)?);

        if !reg0.not_failed() {
            Err(ResponseCode::SegmentDisconnected)
        } else if !reg0.connected() {
            Err(ResponseCode::MuxDisconnected)
        } else {
            Ok(())
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
