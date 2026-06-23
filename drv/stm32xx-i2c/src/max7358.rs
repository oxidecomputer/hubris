// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the MAX7358 I2C mux

use crate::*;
use bitfield::bitfield;
use drv_i2c_api::{ResponseCode, Segment};
use ringbuf::*;
use userlib::*;

pub struct Max7358;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
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
    #[derive(Copy, Clone, Eq, PartialEq)]
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

bitfield! {
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct Configuration(u8);
    preconnect_test_enabled, set_preconnect_test_enabled: 7;
    basic_mode_enabled, set_basic_mode_enabled: 6;
    bus_lockup_disabled, set_bus_lockup_disabled: 5;
    disconnect_locked_only, set_disconnect_locked_only: 4;
    lockup_cleared_on_read, set_lockup_cleared_on_read: 3;
    rst_delay_released, set_rst_delay_released: 2;
    flushout_enabled, set_flushout_enabled: 1;
    interrupt_enabled, set_interrupt_enabled: 0;
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Trace {
    None,
    Read(Register, u8),
    Write(Register, u8),
}

ringbuf!(Trace, 32, Trace::None);

fn read_regs(
    mux: &I2cMux<'_>,
    controller: &I2cController<'_>,
    rbuf: &mut [u8],
) -> Result<(), ResponseCode> {
    let controller_result = controller.write_read(
        mux.address,
        0,
        |_| Some(0),
        ReadLength::Fixed(rbuf.len()),
        |pos, byte| {
            rbuf[pos] = byte;
            Some(())
        },
    );
    match controller_result {
        Err(code) => Err(mux.error_code(code)),
        _ => {
            for (i, &byte) in rbuf.iter().enumerate() {
                ringbuf_entry!(Trace::Read(Register::from(i as u8), byte));
            }

            Ok(())
        }
    }
}

fn write_reg(
    mux: &I2cMux<'_>,
    controller: &I2cController<'_>,
    reg: Register,
    val: u8,
) -> Result<(), ResponseCode> {
    let mut wbuf = [0u8; 3];

    //
    // When doing a write to this bonkers part, unless it's SwitchControl
    // (which is in position 0), we must always write the other two --
    // which necessitates us reading them first.  (Fortunately, we expect
    // writes to SwitchControl to be by far the most frequent!)
    //
    let index = reg as usize;

    if index > 0 {
        read_regs(mux, controller, &mut wbuf[0..index])?;
    }

    ringbuf_entry!(Trace::Write(reg, val));

    wbuf[index] = val;

    match controller.write_read(
        mux.address,
        index + 1,
        |pos| Some(wbuf[pos]),
        ReadLength::Fixed(0),
        |_, _| Some(()),
    ) {
        Err(code) => Err(mux.error_code(code)),
        _ => Ok(()),
    }
}

impl I2cMuxDriver for Max7358 {
    fn configure(
        &self,
        mux: &I2cMux<'_>,
        controller: &I2cController<'_>,
        gpio: &sys_api::Sys,
    ) -> Result<(), ResponseCode> {
        mux.configure(gpio)?;

        //
        // The MAX7358 has a really, really regrettable idea:  it has a
        // "special" (their words) sequence sent to expose enhanced
        // functionality.  The sequence consists of I2C operations that one
        // would never see from a functional initiator:  a zero-byte write
        // followed by a zero-byte read, followed by a zero-byte write,
        // followed by a zero-byte read.  (Because this evokes storied cheat
        // sequences in video games, we choose to call this a "Konami Code.")
        // This is bad enough, but it actually gets worse: this doesn't seem
        // to always work correctly.  In particular, there seem to be modes in
        // which the device confuses the zero-byte read that is the second
        // operation for an *actual* read -- and tries to return the contents
        // of register 0 (which is its defined behavior on a read).  This is
        // not (at all) what the initiator-side is expecting, and, because
        // register 0 is the SwitchControl register which is itself zeroed on
        // reset, this condition results in SDA appearing to be being held low
        // -- and the controller (rightfully) indicates that arbitration is
        // lost.  When this condition has been seen (namely, on hard power
        // on), it is resolved as soon as the initiator emits enough SCL
        // iterations (i.e., controller restarts) for SDA to be let go: a
        // subsequent issuing of the sequence is handled properly in the cases
        // that we've seen.  However, we have also found that issuing a
        // (proper) read ahead of issuing the Konami Code appears to put the
        // part in a better frame of mind -- so we choose to do this, with the
        // hope that it will prevent the caller from needing to reset the
        // controller entirely several times over.
        //
        let mut scratch = [0u8; 1];
        read_regs(mux, controller, &mut scratch[0..1])?;

        controller.send_konami_code(
            mux.address,
            &[
                I2cKonamiCode::Write,
                I2cKonamiCode::Read,
                I2cKonamiCode::Write,
                I2cKonamiCode::Read,
            ],
        )?;

        let reg = SwitchControl(0);
        write_reg(mux, controller, Register::SwitchControl, reg.0)
    }

    fn enable_segment(
        &self,
        mux: &I2cMux<'_>,
        controller: &I2cController<'_>,
        segment: Option<Segment>,
    ) -> Result<(), ResponseCode> {
        let mut reg = SwitchControl(0);

        if let Some(segment) = segment {
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
        }

        write_reg(mux, controller, Register::SwitchControl, reg.0)
    }

    fn reset(
        &self,
        mux: &I2cMux<'_>,
        gpio: &sys_api::Sys,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        mux.reset(gpio)
    }
}
