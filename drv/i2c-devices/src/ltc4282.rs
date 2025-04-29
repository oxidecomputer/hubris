// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the LTC4282 high current hot swap controller

use crate::{CurrentSensor, Validate, VoltageSensor};
use bitfield::bitfield;
use core::cell::Cell;
use drv_i2c_api::*;
use userlib::{
    units::{Amperes, Ohms, Volts},
    FromPrimitive,
};

#[allow(dead_code, non_camel_case_types)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum Register {
    /// Configures On/Off Behavior
    CONTROL = 0x00,

    /// Enables Alerts
    ALERT = 0x02,

    /// Logs Faults
    FAULT_LOG = 0x04,

    /// Logs ADC Alerts
    ADC_ALERT_LOG = 0x05,

    /// Selects FET-BAD Fault Timeout
    FET_BAD_FAULT_TIME = 0x06,

    /// Configures GPIO Outputs
    GPIO_CONFIG = 0x07,

    /// Threshold For Min Alarm on VGPIO
    VGPIO_ALARM_MIN = 0x08,

    /// Threshold for Max Alarm on VGPIO
    VGPIO_ALARM_MAX = 0x09,

    /// Threshold for Min Alarm on VSOURCE
    VSOURCE_ALARM_MIN = 0x0A,

    /// Threshold for Max Alarm on VSOURCE
    VSOURCE_ALARM_MAX = 0x0B,

    /// Threshold for Min Alarm on VSENSE
    VSENSE_ALARM_MIN = 0x0C,

    /// Threshold for Max Alarm on VSENSE
    VSENSE_ALARM_MAX = 0x0D,

    /// Threshold for Min Alarm on POWER
    POWER_ALARM_MIN = 0x0E,

    /// Threshold for Max Alarm on POWER
    POWER_ALARM_MAX = 0x0F,

    /// Division Factor for External Clock
    CLOCK_DIVIDER = 0x10,

    /// Adjusts Current Limit Value
    ILIM_ADJUST = 0x11,

    /// Meters Energy Delivered to Load
    ENERGY = 0x12,

    /// Counts Power Delivery Time
    TIME_COUNTER = 0x18,

    /// Clear Alerts, Force ALERT Pin Low
    ALERT_CONTROL = 0x1C,

    /// Control ADC, Energy Meter
    ADC_CONTROL = 0x1D,

    /// Fault and Pin Status
    STATUS = 0x1E,

    /// EEPROM Default
    EE_CONTROL = 0x20,

    /// EEPROM Default
    EE_ALERT = 0x22,

    /// EEPROM Default
    EE_FAULT_LOG = 0x24,

    /// EEPROM Default
    EE_ADC_ALERT_LOG = 0x25,

    /// EEPROM Default
    EE_FET_BAD_FAULT_TIME = 0x26,

    /// EEPROM Default
    EE_GPIO_CONFIG = 0x27,

    /// EEPROM Default
    EE_VGPIO_ALARM_MIN = 0x28,

    /// EEPROM Default
    EE_VGPIO_ALARM_MAX = 0x29,

    /// EEPROM Default
    EE_VSOURCE_ALARM_MIN = 0x2A,

    /// EEPROM Default
    EE_VSOURCE_ALARM_MAX = 0x2B,

    /// EEPROM Default
    EE_VSENSE_ALARM_MIN = 0x2C,

    /// EEPROM Default
    EE_VSENSE_ALARM_MAX = 0x2D,

    /// EEPROM Default
    EE_POWER_ALARM_MIN = 0x2E,

    /// EEPROM Default
    EE_POWER_ALARM_MAX = 0x2F,

    /// EEPROM Default
    EE_CLOCK_DIVIDER = 0x30,

    /// EEPROM Default
    EE_ILIM_ADJUST = 0x31,

    /// Most Recent ADC Result for VGPIO
    VGPIO = 0x34,

    /// Min ADC Result for VGPIO
    VGPIO_MIN = 0x36,

    /// Max ADC Result for VGPIO
    VGPIO_MAX = 0x38,

    /// Most Recent ADC Result for VSOURCE
    VSOURCE = 0x3A,

    /// Min ADC Result for VSOURCE
    VSOURCE_MIN = 0x3C,

    /// Max ADC Result for VSOURCE
    VSOURCE_MAX = 0x3E,

    /// Most Recent ADC Result for VSENSE
    VSENSE = 0x40,

    /// Min ADC Result for VSENSE
    VSENSE_MIN = 0x42,

    /// Max ADC Result for VSENSE
    VSENSE_MAX = 0x44,

    /// Most Recent ADC Result for POWER
    POWER = 0x46,

    /// Min ADC Result for POWER
    POWER_MIN = 0x48,

    /// Max ADC Result for POWER
    POWER_MAX = 0x4A,

    /// Spare EEPROM Memory
    EE_SCRATCH_PAD = 0x4C,
}

#[derive(Copy, Clone, PartialEq, FromPrimitive)]
#[repr(u8)]
pub enum Mode {
    Mode3P3V = 0b00,
    Mode5V = 0b01,
    Mode12V = 0b10,
    Mode24V = 0b11,
}

pub enum Threshold {
    External = 0b00,
    Threshold5Percent = 0b01,
    Threshold10Percent = 0b10,
    Threshold15Percent = 0b11,
}

bitfield! {
    #[derive(Copy, Clone)]
    pub struct Control(u16);
    on_fault_mask, set_on_fault_mask: 15;
    on_delay, set_on_delay: 14;
    on_enb, set_on_enb: 13;
    mass_write_enable, set_mass_write_enable: 12;
    fet_on, set_fet_on: 11;
    oc_autoretry, set_oc_autoretry: 10;
    uv_autoretry, set_uv_autoretry: 9;
    ov_autoretry, set_ov_autoretry: 8;
    fb_mode, set_fb_mode: 7, 6;
    uv_mode, set_uv_mode: 5, 4;
    ov_mode, set_ov_mode: 3, 2;
    vin_mode, set_vin_mode: 1, 0;
}

pub struct Ltc4282 {
    device: I2cDevice,
    control: Cell<Option<Control>>,
    rsense: Ohms,
}

impl Ltc4282 {
    pub fn new(device: &I2cDevice, rsense: Ohms) -> Self {
        Self {
            device: *device,
            rsense,
            control: Cell::new(None),
        }
    }

    pub fn read_reg(&self, reg: Register) -> Result<u8, ResponseCode> {
        self.device.read_reg::<u8, u8>(reg as u8)
    }

    pub fn read_reg16(&self, reg: Register) -> Result<u16, ResponseCode> {
        let val = self.device.read_reg::<u8, [u8; 2]>(reg as u8)?;
        let (msb, lsb) = (val[0] as u16, val[1] as u16);

        Ok((msb << 8) | lsb)
    }

    fn read_control(&self) -> Result<Control, ResponseCode> {
        if let Some(ref control) = self.control.get() {
            return Ok(*control);
        }

        let control = Control(self.read_reg16(Register::CONTROL)?);
        self.control.set(Some(control));

        Ok(control)
    }

    //
    // Returns Vfs(out), the ADC full-scale range (which depends on the mode)
    //
    fn vfs_out(&self) -> Result<f32, ResponseCode> {
        let control = self.read_control()?;
        Ok(match Mode::from_u16(control.vin_mode()).unwrap() {
            Mode::Mode3P3V => 5.547,
            Mode::Mode5V => 8.32,
            Mode::Mode12V => 16.64,
            Mode::Mode24V => 33.28,
        })
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }
}

impl Validate<ResponseCode> for Ltc4282 {
    fn validate(device: &I2cDevice) -> Result<bool, ResponseCode> {
        let control = Ltc4282::new(device, Ohms(0.0)).read_control()?;

        //
        // We don't have any identification bits, so we look at the most
        // distinctive bits we can depend on:  the default disposition of the
        // control bits.  (If we change these dynamically, this logic will
        // naturally need to change.)  We deliberately do not depend on the
        // mode settings because these can be strapped to different values.
        //
        #[allow(clippy::bool_comparison)]
        Ok(control.on_fault_mask() == true
            && control.on_delay() == false
            && control.on_enb() == true
            && control.mass_write_enable() == true
            && control.oc_autoretry() == false
            && control.uv_autoretry() == true
            && control.ov_autoretry() == true)
    }
}

impl VoltageSensor<ResponseCode> for Ltc4282 {
    fn read_vout(&self) -> Result<Volts, ResponseCode> {
        let vfs = self.vfs_out()?;
        let reading = self.read_reg16(Register::VSOURCE)?;

        //
        // Following the formula under "Data Converters" in the datasheet
        //
        Ok(Volts((reading as f32 * vfs) / ((1u32 << 16) - 1) as f32))
    }
}

impl CurrentSensor<ResponseCode> for Ltc4282 {
    fn read_iout(&self) -> Result<Amperes, ResponseCode> {
        let reading = self.read_reg16(Register::VSENSE)? as f32;
        let divisor = ((1u32 << 16) - 1) as f32 * self.rsense.0;

        //
        // Following the formula under "Data Converters" in the datasheet
        //
        Ok(Amperes((reading * 0.040) / divisor))
    }
}
