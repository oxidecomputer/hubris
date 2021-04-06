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

bitfield! {
    #[derive(Copy, Clone, PartialEq)]
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

ringbuf!((Option<Register>, u8), 32, (None, 0));

fn read_regs(
    mux: &I2cMux,
    controller: &I2cController,
    rbuf: &mut [u8],
    ctrl: &I2cControl,
) -> Result<(), ResponseCode> {
    match controller.write_read(
        mux.address,
        0,
        |_| Some(0),
        rbuf.len(),
        |pos, byte| {
            rbuf[pos] = byte;
            Some(())
        },
        ctrl,
    ) {
        Err(code) => Err(mux.error_code(code)),
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
    ctrl: &I2cControl,
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
        read_regs(mux, controller, &mut wbuf[0..index], ctrl)?;
    }

    ringbuf_entry!((Some(reg), val));

    wbuf[index] = val;

    match controller.write_read(
        mux.address,
        index + 1,
        |pos| Some(wbuf[pos]),
        0,
        |_, _| Some(()),
        ctrl,
    ) {
        Err(code) => Err(mux.error_code(code)),
        _ => Ok(()),
    }
}

impl I2cMuxDriver for Max7358 {
    fn configure(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        gpio: &drv_stm32h7_gpio_api::Gpio,
        ctrl: &I2cControl,
    ) -> Result<(), ResponseCode> {
        controller.special(
            mux.address,
            &[
                I2cSpecial::Write,
                I2cSpecial::Read,
                I2cSpecial::Write,
                I2cSpecial::Read,
            ],
            ctrl,
        )?;

        let reg = SwitchControl(0);
        write_reg(mux, controller, Register::SwitchControl, reg.0, ctrl)?;

        //
        // The MAX7358 seems to trigger a bus lockup if it detects that
        // its upstream side has locked -- which is not necessarily accurate
        // if the upstream side is a TCA9802/TCA9517 pair.  We disable lockup
        // detection for now to allow this config to function.
        //
        let mut reg = Configuration(0);
        reg.set_bus_lockup_disabled(true);

        write_reg(mux, controller, Register::Configuration, reg.0, ctrl)?;

        mux.configure(gpio)
    }

    fn enable_segment(
        &self,
        mux: &I2cMux,
        controller: &I2cController,
        segment: Segment,
        ctrl: &I2cControl,
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
        }

        write_reg(mux, controller, Register::SwitchControl, reg.0, ctrl)
    }

    fn reset(
        &self,
        mux: &I2cMux,
        gpio: &drv_stm32h7_gpio_api::Gpio,
    ) -> Result<(), drv_i2c_api::ResponseCode> {
        mux.reset(gpio)
    }
}
