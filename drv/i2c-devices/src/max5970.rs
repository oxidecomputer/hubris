// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for the MAX5970 hot swap controller

use crate::{CurrentSensor, Validate, VoltageSensor};
use drv_i2c_api::*;
use num_traits::float::FloatCore;
use userlib::units::*;
use userlib::*;

#[allow(dead_code, non_camel_case_types)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, FromPrimitive)]
pub enum Register {
    /// High 8 bits ([9:2]) of latest current-signal
    adc_chx_cs_msb_ch1 = 0x00,

    /// Low 2 bits ([1:0]) of latest current-signal ADC
    adc_chx_cs_lsb_ch1 = 0x01,

    /// High 8 bits ([9:2]) of latest voltage-signal
    adc_chx_mon_msb_ch1 = 0x02,

    /// Low 2 bits ([1:0]) of latest voltage-signal
    adc_chx_mon_lsb_ch1 = 0x03,

    /// High 8 bits ([9:2]) of latest current-signal
    adc_chx_cs_msb_ch2 = 0x04,

    /// Low 2 bits ([1:0]) of latest current-signal ADC
    adc_chx_cs_lsb_ch2 = 0x05,

    /// High 8 bits ([9:2]) of latest voltage-signal
    adc_chx_mon_msb_ch2 = 0x06,

    /// Low 2 bits ([1:0]) of latest voltage-signal
    adc_chx_mon_lsb_ch2 = 0x07,

    /// High 8 bits ([9:2]) of current-signal minimum
    min_chx_cs_msb_ch1 = 0x08,

    /// Low 2 bits ([1:0]) of current-signal minimum
    min_chx_cs_lsb_ch1 = 0x09,

    /// High 8 bits ([9:2]) of current-signal maximum
    max_chx_cs_msb_ch1 = 0x0a,

    /// Low 2 bits ([1:0]) of current-signal maximum
    max_chx_cs_lsb_ch1 = 0x0b,

    /// High 8 bits ([9:2]) of voltage-signal minimum
    min_chx_mon_msb_ch1 = 0x0c,

    /// Low 2 bits ([1:0]) of voltage-signal minimum
    min_chx_mon_lsb_ch1 = 0x0d,

    /// High 8 bits ([9:2]) of voltage-signal maximum
    max_chx_mon_msb_ch1 = 0x0e,

    /// Low 2 bits ([1:0]) of voltage-signal maximum
    max_chx_mon_lsb_ch1 = 0x0f,

    /// High 8 bits ([9:2]) of current-signal minimum
    min_chx_cs_msb_ch2 = 0x10,

    /// Low 2 bits ([1:0]) of current-signal minimum
    min_chx_cs_lsb_ch2 = 0x11,

    /// High 8 bits ([9:2]) of current-signal maximum
    max_chx_cs_msb_ch2 = 0x12,

    /// Low 2 bits ([1:0]) of current-signal maximum
    max_chx_cs_lsb_ch2 = 0x13,

    /// High 8 bits ([9:2]) of voltage-signal minimum
    min_chx_mon_msb_ch2 = 0x14,

    /// Low 2 bits ([1:0]) of voltage-signal minimum
    min_chx_mon_lsb_ch2 = 0x15,

    /// High 8 bits ([9:2]) of voltage-signal maximum
    max_chx_mon_msb_ch2 = 0x16,

    /// Low 2 bits ([1:0]) of voltage-signal maximum
    max_chx_mon_lsb_ch2 = 0x17,

    /// MON input range setting
    mon_range = 0x18,

    /// Selective enabling of circular buffer
    cbuf_chx_store = 0x19,

    /// High 8 bits ([9:2]) of undervoltage warning
    uv1thr_chx_msb_ch1 = 0x1a,

    /// Low 2 bits ([1:0]) of undervoltage warning
    uv1thr_chx_lsb_ch1 = 0x1b,

    /// High 8 bits ([9:2]) of undervoltage critical
    uv2thr_chx_msb_ch1 = 0x1c,

    /// Low 2 bits ([1:0]) of undervoltage critical
    uv2thr_chx_lsb_ch1 = 0x1d,

    /// High 8 bits ([9:2]) of overvoltage warning
    ov1thr_chx_msb_ch1 = 0x1e,

    /// Low 2 bits ([1:0]) of overvoltage warning
    ov1thr_chx_lsb_ch1 = 0x1f,

    /// High 8 bits ([9:2]) of overvoltage critical
    ov2thr_chx_msb_ch1 = 0x20,

    /// Low 2 bits ([1:0]) of overvoltage critical
    ov2thr_chx_lsb_ch1 = 0x21,

    /// High 8 bits ([9:2]) of overcurrent warning
    oithr_chx_msb_ch1 = 0x22,

    /// Low 2 bits ([1:0]) of overcurrent warning
    oithr_chx_lsb_ch1 = 0x23,

    /// High 8 bits ([9:2]) of undervoltage warning
    uv1thr_chx_msb_ch2 = 0x24,

    /// Low 2 bits ([1:0]) of undervoltage warning
    uv1thr_chx_lsb_ch2 = 0x25,

    /// High 8 bits ([9:2]) of undervoltage critical
    uv2thr_chx_msb_ch2 = 0x26,

    /// Low 2 bits ([1:0]) of undervoltage critical
    uv2thr_chx_lsb_ch2 = 0x27,

    /// High 8 bits ([9:2]) of overvoltage warning
    ov1thr_chx_msb_ch2 = 0x28,

    /// Low 2 bits ([1:0]) of overvoltage warning
    ov1thr_chx_lsb_ch2 = 0x29,

    /// High 8 bits ([9:2]) of overvoltage critical
    ov2thr_chx_msb_ch2 = 0x2a,

    /// Low 2 bits ([1:0]) of overvoltage critical
    ov2thr_chx_lsb_ch2 = 0x2b,

    /// High 8 bits ([9:2]) of overcurrent warning
    oithr_chx_msb_ch2 = 0x2c,

    /// Low 2 bits ([1:0]) of overcurrent warning
    oithr_chx_lsb_ch2 = 0x2d,

    /// Fast-comparator threshold DAC setting
    dac_chx_fast = 0x2e,

    /// Current threshold fast-to-slow ratio setting
    ifast2slow = 0x30,

    /// Slow-trip and fast-trip comparators status register
    status0 = 0x31,

    /// PROT, MODE, and ON_ inputs status register
    status1 = 0x32,

    /// Fast-trip threshold maximum range setting bits
    status2 = 0x33,

    /// LATCH, POL, ALERT, and PG_ status register
    status3 = 0x34,

    /// Status register for undervoltage detection (warning or critical)
    fault0 = 0x35,

    /// Status register for overvoltage detection (warning or critical)
    fault1 = 0x36,

    /// Status register for overcurrent detection (warning)
    fault2 = 0x37,

    /// Delay setting between MON measurement and PG_ assertion
    pgdly = 0x38,

    /// Load register with 0xA5 to enable force-on function
    fokey = 0x39,

    /// Register that enables force-on function for a channel
    foset = 0x3a,

    /// Channel enable bits
    chxen = 0x3b,

    /// OC deglitch enable bits
    dgl_i = 0x3c,

    /// UV deglitch enable bits
    dgl_uv = 0x3d,

    /// OV deglitch enable bits
    dgl_ov = 0x3e,

    /// Circular buffers readout mode: 8 bit or 10 bit
    cbufrd_hibyonly = 0x3f,

    /// Circular buffer stop-delay
    cbuf_dly_stop = 0x40,

    /// Reset control bits for peak-detection registers
    peak_log_rst = 0x41,

    /// Hold control bits for peak-detection registers
    peak_log_hold = 0x42,

    /// Base address for block read of 50-sample voltage-signal data buffer
    cubf_ba_chx_v = 0x46,

    /// Base address for block read of 50-sample current-signal data buffer
    cubf_ba_chx_i = 0x47,
}

struct MonRange(u8);

impl MonRange {
    fn full_scale_voltage(&self, rail: u8) -> u8 {
        let range = if rail == 0 {
            self.0 & 0b11
        } else {
            (self.0 >> 2) & 0b11
        };

        match range {
            0b00 => 16,
            0b01 => 8,
            0b10 => 4,
            0b11 => 2,
            _ => unreachable!(),
        }
    }
}

struct Status2(u8);

impl Status2 {
    fn max_current_sense_range(&self, rail: u8) -> Option<u8> {
        //
        // The datasheet is enragingly inconsistent about how it refers to the
        // channels.  For most registers that have different settings for
        // channels, it refers to them as Channel 1 and Channel 2 -- except
        // for status2, which refers to Channel 0 and Channel 1.
        //
        let range = if rail == 0 {
            self.0 & 0b11
        } else {
            (self.0 >> 2) & 0b11
        };

        //
        // Our maximum current-sense range is 25mV, 50mV, or 100mV. (Contrary
        // to the implication of the datasheet, there is no fourth maximum
        // current-sense range.)
        //
        match range {
            0b00 => Some(100),
            0b01 => Some(50),
            0b10 => Some(25),
            _ => None,
        }
    }
}

pub struct Max5970 {
    device: I2cDevice,
    rail: u8,
    rsense: i32,
}

impl Max5970 {
    pub fn new(device: &I2cDevice, rail: u8, rsense: Ohms) -> Self {
        Self {
            device: *device,
            rail,
            rsense: (rsense.0 * 1000.0).round() as i32,
        }
    }

    pub fn read_reg(&self, reg: Register) -> Result<u8, ResponseCode> {
        self.device.read_reg::<u8, u8>(reg as u8)
    }

    pub fn i2c_device(&self) -> &I2cDevice {
        &self.device
    }

    fn convert_volts(&self, mon_range: MonRange, msb: u8, lsb: u8) -> Volts {
        //
        // The 10-bit value from the ADC is a fraction of the full-scale
        // voltage setting.
        //
        let divisor = 1024.0 / mon_range.full_scale_voltage(self.rail) as f32;

        Volts(((((msb as u16) << 2) | (lsb as u16)) as f32) / divisor)
    }

    fn convert_current(
        &self,
        status2: Status2,
        msb: u8,
        lsb: u8,
    ) -> Result<Amperes, ResponseCode> {
        let millivolts = status2
            .max_current_sense_range(self.rail)
            .ok_or(ResponseCode::BadDeviceState)?;

        //
        // The 10-bit value from the ADC is a fraction of the maximum
        // current-sense range.
        //
        let divisor = 1024.0 / millivolts as f32;
        let delta = ((((msb as u16) << 2) | (lsb as u16)) as f32) / divisor;

        //
        // We have the voltage drop across the current sense resistor; to
        // determine current, we divide voltage by resistance (I = V / R).
        //
        Ok(Amperes(delta / self.rsense as f32))
    }

    pub fn max_vout(&self) -> Result<Volts, ResponseCode> {
        let (msb, lsb) = if self.rail == 0 {
            (
                self.read_reg(Register::max_chx_mon_msb_ch1)?,
                self.read_reg(Register::max_chx_mon_lsb_ch1)?,
            )
        } else {
            (
                self.read_reg(Register::max_chx_mon_msb_ch2)?,
                self.read_reg(Register::max_chx_mon_lsb_ch2)?,
            )
        };

        let mon_range = MonRange(self.read_reg(Register::mon_range)?);
        Ok(self.convert_volts(mon_range, msb, lsb))
    }

    pub fn max_iout(&self) -> Result<Amperes, ResponseCode> {
        let (msb, lsb) = if self.rail == 0 {
            (
                self.read_reg(Register::max_chx_cs_msb_ch1)?,
                self.read_reg(Register::max_chx_cs_lsb_ch1)?,
            )
        } else {
            (
                self.read_reg(Register::max_chx_cs_msb_ch2)?,
                self.read_reg(Register::max_chx_cs_lsb_ch2)?,
            )
        };

        let status2 = Status2(self.read_reg(Register::status2)?);
        self.convert_current(status2, msb, lsb)
    }

    pub fn status0(&self) -> Result<u8, ResponseCode> {
        self.read_reg(Register::status0)
    }
}

impl Validate<ResponseCode> for Max5970 {
    fn validate(device: &I2cDevice) -> Result<bool, ResponseCode> {
        let val = Max5970::new(device, 0, Ohms(0.0))
            .read_reg(Register::cbuf_dly_stop)?;
        Ok(val == 0x19)
    }
}

impl VoltageSensor<ResponseCode> for Max5970 {
    fn read_vout(&self) -> Result<Volts, ResponseCode> {
        let (msb, lsb) = if self.rail == 0 {
            (
                self.read_reg(Register::adc_chx_mon_msb_ch1)?,
                self.read_reg(Register::adc_chx_mon_lsb_ch1)?,
            )
        } else {
            (
                self.read_reg(Register::adc_chx_mon_msb_ch2)?,
                self.read_reg(Register::adc_chx_mon_lsb_ch2)?,
            )
        };

        let mon_range = MonRange(self.read_reg(Register::mon_range)?);
        Ok(self.convert_volts(mon_range, msb, lsb))
    }
}

impl CurrentSensor<ResponseCode> for Max5970 {
    fn read_iout(&self) -> Result<Amperes, ResponseCode> {
        let (msb, lsb) = if self.rail == 0 {
            (
                self.read_reg(Register::adc_chx_cs_msb_ch1)?,
                self.read_reg(Register::adc_chx_cs_lsb_ch1)?,
            )
        } else {
            (
                self.read_reg(Register::adc_chx_cs_msb_ch2)?,
                self.read_reg(Register::adc_chx_cs_lsb_ch2)?,
            )
        };

        let status2 = Status2(self.read_reg(Register::status2)?);
        self.convert_current(status2, msb, lsb)
    }
}
