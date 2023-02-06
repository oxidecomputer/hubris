// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the `power` server.

#![no_std]

pub use drv_i2c_api::ResponseCode;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::sys_send;
use zerocopy::{AsBytes, FromBytes};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, SerializedSize)]
pub enum Device {
    PowerShelf,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, SerializedSize)]
pub enum Operation {
    FanConfig1_2,
    FanCommand1,
    FanCommand2,
    IoutOcFaultLimit,
    IoutOcWarnLimit,
    OtWarnLimit,
    IinOcWarnLimit,
    PoutOpWarnLimit,
    PinOpWarnLimit,
    StatusByte,
    StatusWord,
    StatusVout,
    StatusIout,
    StatusInput,
    StatusTemperature,
    StatusCml,
    StatusMfrSpecific,
    StatusFans1_2,
    ReadEin,
    ReadEout,
    ReadVin,
    ReadIin,
    ReadVcap,
    ReadVout,
    ReadIout,
    ReadTemperature1,
    ReadTemperature2,
    ReadTemperature3,
    ReadFanSpeed1,
    ReadFanSpeed2,
    ReadPout,
    ReadPin,
    PmbusRevision,
    MfrId,
    MfrModel,
    MfrRevision,
    MfrLocation,
    MfrDate,
    MfrSerial,
    MfrVinMin,
    MfrVinMax,
    MfrIinMax,
    MfrPinMax,
    MfrVoutMin,
    MfrVoutMax,
    MfrIoutMax,
    MfrPoutMax,
    MfrTambientMax,
    MfrTambientMin,
    MfrEfficiencyHl,
    MfrMaxTemp1,
    MfrMaxTemp2,
    MfrMaxTemp3,
}

pub const MAX_BLOCK_LEN: usize = 17;

// We use a `u8` for the actual block length; ensure `MAX_BLOCK_LEN` fits.
static_assertions::const_assert!(MAX_BLOCK_LEN <= u8::MAX as usize);

#[derive(Debug, Clone, Deserialize, Serialize, SerializedSize)]
pub enum PmbusValue {
    Celsius(f32),
    Amperes(f32),
    Watts(f32),
    Volts(f32),
    Rpm(f32),
    Raw8(u8),
    Raw16(u16),
    Percent(f32),
    Block { data: [u8; MAX_BLOCK_LEN], len: u8 },
}

impl From<pmbus::units::Celsius> for PmbusValue {
    fn from(value: pmbus::units::Celsius) -> Self {
        Self::Celsius(value.0)
    }
}

impl From<pmbus::units::Amperes> for PmbusValue {
    fn from(value: pmbus::units::Amperes) -> Self {
        Self::Amperes(value.0)
    }
}

impl From<pmbus::units::Watts> for PmbusValue {
    fn from(value: pmbus::units::Watts) -> Self {
        Self::Watts(value.0)
    }
}

impl From<pmbus::units::Volts> for PmbusValue {
    fn from(value: pmbus::units::Volts) -> Self {
        Self::Volts(value.0)
    }
}

impl From<pmbus::units::Rpm> for PmbusValue {
    fn from(value: pmbus::units::Rpm) -> Self {
        Self::Rpm(value.0)
    }
}

impl From<pmbus::units::Percent> for PmbusValue {
    fn from(value: pmbus::units::Percent) -> Self {
        Self::Percent(value.0)
    }
}

/// Simple wrapper type for the BMR491 event log
///
/// To simplify the implementation, this is the result of a raw PMBus read;
/// this means the first byte is the length of the remaining data (i.e. 23).
#[derive(
    Debug,
    Clone,
    Copy,
    Deserialize,
    Serialize,
    SerializedSize,
    AsBytes,
    FromBytes,
)]
#[repr(C)]
pub struct Bmr491Event([u8; 24]);

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
