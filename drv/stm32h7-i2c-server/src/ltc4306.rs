//! Driver for the LTC4306 I2C mux

use drv_i2c_api::{Port, Segment, ResponseCode};
use drv_stm32h7_i2c::*;
use ringbuf::*;
use bitfield::bitfield;

bitfield! {
    #[derive(Copy, Clone, PartialEq)]
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
    #[derive(Copy, Clone, PartialEq)]
    pub struct Register1(u8);
    upstream_accelerators_enable, set_upstream_accelerators_enable: 7;
    downstream_accelerators_enable, set_downstream_accelerators_enable: 6;
    gpio1_output_state, set_gpio1_output_state: 5;
    gpio2_output_state, set_gpio2_output_state: 4;
    gpio1_logic_state, _: 1;
    gpio2_logic_state, _: 0;
}

enum Timeout {
    TimeoutDisabled = 0b00,
    Timeout30ms = 0b01,
    Timeout15ms = 0b10,
    Timeout7point5ms = 0b11,
}

bitfield! {
    #[derive(Copy, Clone, PartialEq)]
    pub struct Register2(u8);
    gpio1_mode_input, set_gpio1_mode_input: 7;
    gpio2_mode_input, set_gpio2_mode_input: 6;
    connect_regardless, set_connect_regardless: 5;
    gpio1_push_pull, set_gpio1_push_pull: 4;
    gpio2_push_pull, set_gpio2_push_pull: 3;
    mass_write_enabled, set_mass_write_enabled: 2;
    timeout, set_timeout: 1, 0;
}

bitfield! {
    #[derive(Copy, Clone, PartialEq)]
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
    mux: &I2cMux, 
    controller: &I2cController,
    reg: u8,
    mut enable: impl FnMut(u32),
    mut wfi: impl FnMut(u32)
) -> Result<u8, ResponseCode> {
    let mut rval = [0u8; 1];
    let wlen = 1;

    match controller.write_read(
        mux.address,
        wlen, |_| reg,
        rval.len(), |_, byte| { rval[0] = byte },
        enable,
        wfi
    ) {
        Err(code) => Err(match code {
            I2cError::NoDevice => ResponseCode::BadMuxAddress,
            I2cError::NoRegister => ResponseCode::BadMuxRegister,
        }),
        _ => {
            Ok(rval[0])
        }
    }
}

fn write_reg_u8(
    mux: &I2cMux, 
    controller: &I2cController,
    reg: u8,
    val: u8,
    mut enable: impl FnMut(u32),
    mut wfi: impl FnMut(u32)
) -> Result<(), ResponseCode> {
    match controller.write_read(
        mux.address,
        2, |pos| if pos == 0 { reg } else { val },
        0, |_, _| {},
        enable,
        wfi
    ) {
        Err(code) => Err(match code {
            I2cError::NoDevice => ResponseCode::BadMuxAddress,
            I2cError::NoRegister => ResponseCode::BadMuxRegister,
        }),
        _ => {
            Ok(())
        }
    }
}

pub fn ltc4306_enable_segment(
    mux: &I2cMux,
    controller: &I2cController,
    segment: Segment,
    mut enable: impl FnMut(u32),
    mut wfi: impl FnMut(u32),
) -> Result<(), ResponseCode> {
    // let reg0 = Register0(read_reg_u8(mux, controller, 0, enable, wfi)?);

    let mut reg3 = Register3(0);

    match segment {
        Segment::S1 => { reg3.set_bus1_connected(true); }
        Segment::S2 => { reg3.set_bus2_connected(true); }
        Segment::S3 => { reg3.set_bus3_connected(true); }
        Segment::S4 => { reg3.set_bus4_connected(true); }
    }

    write_reg_u8(mux, controller, 3, reg3.0, enable, wfi)?;

    Ok(())
}
