//! Driver for the MAX7358 I2C mux

use crate::*;
use bitfield::bitfield;
use drv_i2c_api::{ResponseCode, Segment};
use ringbuf::*;
use userlib::*;

pub struct Max7358;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
enum Register {
    SwitchControl = 0x00,
    Configuration = 0x01,
    FlushOutSequence = 0x02,
    LockupIndication = 0x03,
    TrafficPriorToLockupByte0 = 0x04,
    TrafficPriorToLockupByte1 = 0x05,
    StuckHighFault = 0x06,
}

impl From<u8> for Register {
    fn from(value: u8) -> Self {
        Register::from_u8(value).unwrap()
    }
}

impl From<Register> for u8 {
    fn from(value: Register) -> Self {
        value as u8
    }
}

bitfield! {
    #[derive(Copy, Clone, PartialEq)]
    pub struct SwitchControl(u8);
    channel7_selected, set_channel7_selected: 7;
    channel6_selected, set_channel6_selected: 6;
    channel5_selected, set_channel5_selected: 5;
    channel4_selected, set_channel4_selected: 4;
    channel3_selected, set_channel3_selected: 3;
    channel2_selected, set_channel2_selected: 2;
    channel1_selected, set_channel1_selected: 1;
    channel0_selected, set_channel0_selected: 0;
}

ringbuf!((Option<Register>, u8), 8, (None, 0));

fn read_regs(
    mux: &I2cMux,
    controller: &I2cController,
    rbuf: &mut [u8],
    enable: impl FnMut(u32) + Copy,
    wfi: impl FnMut(u32) + Copy,
) -> Result<(), ResponseCode> {
    match controller.write_read(
        mux.address,
        0,
        |_| 0,
        rbuf.len(),
        |pos, byte| rbuf[pos] = byte,
        enable,
        wfi,
    ) {
        Err(code) => Err(match code {
            ResponseCode::NoDevice => ResponseCode::BadMuxAddress,
            ResponseCode::NoRegister => ResponseCode::BadMuxRegister,
            ResponseCode::BusLocked => ResponseCode::BusLockedMux,
            ResponseCode::BusReset => ResponseCode::BusResetMux,
            _ => code,
        }),
        _ => {
            for i in 0..rbuf.len() {
                ringbuf_entry!((Some(Register::from(i as u8)), rbuf[i]));
            }

            Ok(())
        }
    }
}

fn write_reg(
    mux: &I2cMux,
    controller: &I2cController,
    reg: Register,
    val: u8,
    enable: impl FnMut(u32) + Copy,
    wfi: impl FnMut(u32) + Copy,
) -> Result<(), ResponseCode> {
    let mut wbuf = [0u8; 3];

    //
    // When doing a write to this bonkers part, unless it's SwitchControl
    // (which is in position 0), we must always write the other two --
    // which necessitates us reading them first.
    //
    let index = reg as usize;

    if index > 0 {
        read_regs(mux, controller, &mut wbuf[0..index], enable, wfi)?;
    }

    ringbuf_entry!((Some(reg), val));

    wbuf[index] = val;

    match controller.write_read(
        mux.address,
        index + 1,
        |pos| wbuf[pos],
        0,
        |_, _| {},
        enable,
        wfi,
    ) {
        Err(code) => Err(match code {
            ResponseCode::NoDevice => ResponseCode::BadMuxAddress,
            ResponseCode::NoRegister => ResponseCode::BadMuxRegister,
            ResponseCode::BusLocked => ResponseCode::BusLockedMux,
            ResponseCode::BusReset => ResponseCode::BusResetMux,
            _ => code,
        }),
        _ => Ok(()),
    }
}

impl Max7358 {
    pub fn configure(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        enable: impl FnMut(u32) + Copy,
        wfi: impl FnMut(u32) + Copy,
    ) -> Result<(), ResponseCode> {
        controller.special(
            mux.address,
            &[
                I2cSpecial::Write,
                I2cSpecial::Read,
                I2cSpecial::Write,
                I2cSpecial::Read,
            ],
            enable,
            wfi,
        )?;

        let reg = SwitchControl(0);
        write_reg(mux, controller, Register::SwitchControl, reg.0, enable, wfi)
    }

    pub fn enable_segment(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        segment: Segment,
        enable: impl FnMut(u32) + Copy,
        wfi: impl FnMut(u32) + Copy,
    ) -> Result<(), ResponseCode> {
        let mut reg = SwitchControl(0);

        match segment {
            Segment::S1 => {
                reg.set_channel0_selected(true);
            }
            Segment::S2 => {
                reg.set_channel1_selected(true);
            }
            Segment::S3 => {
                reg.set_channel2_selected(true);
            }
            Segment::S4 => {
                reg.set_channel3_selected(true);
            }
            Segment::S5 => {
                reg.set_channel4_selected(true);
            }
            Segment::S6 => {
                reg.set_channel5_selected(true);
            }
            Segment::S7 => {
                reg.set_channel6_selected(true);
            }
            Segment::S8 => {
                reg.set_channel7_selected(true);
            }
            _ => {
                return Err(ResponseCode::SegmentNotFound);
            }
        }

        write_reg(mux, controller, Register::SwitchControl, reg.0, enable, wfi)
    }
}
