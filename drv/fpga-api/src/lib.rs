// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the FPGA server.

#![no_std]

use core::ops::Deref;

use drv_spi_api::SpiError;
use userlib::*;
use zerocopy::{AsBytes, FromBytes};

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FpgaError {
    ImplError(u8),
    BitstreamError(u8),
    InvalidState,
    InvalidValue,
    PortDisabled,
    BadDevice,
    NotLocked,
    AlreadyLocked,
}

impl From<FpgaError> for u16 {
    fn from(e: FpgaError) -> Self {
        match e {
            FpgaError::ImplError(error_code) => 0x0100 | (error_code as u16),
            FpgaError::BitstreamError(error_code) => {
                0x0200 | (error_code as u16)
            }
            FpgaError::InvalidState => 0x0300,
            FpgaError::InvalidValue => 0x0301,
            FpgaError::PortDisabled => 0x0400,
            FpgaError::BadDevice => 0x0500,
            FpgaError::NotLocked => 0x0501,
            FpgaError::AlreadyLocked => 0x0502,
        }
    }
}

impl From<SpiError> for FpgaError {
    fn from(e: SpiError) -> Self {
        FpgaError::ImplError(u32::from(e) as u8)
    }
}

impl From<FpgaError> for u32 {
    fn from(e: FpgaError) -> Self {
        u16::from(e) as u32
    }
}

impl core::convert::TryFrom<u16> for FpgaError {
    type Error = ();

    fn try_from(v: u16) -> Result<Self, Self::Error> {
        match v & 0xff00 {
            0x0100 => Ok(FpgaError::ImplError(v as u8)),
            0x0200 => Ok(FpgaError::BitstreamError(v as u8)),
            _ => match v {
                0x0300 => Ok(FpgaError::InvalidState),
                0x0301 => Ok(FpgaError::InvalidValue),
                0x0400 => Ok(FpgaError::PortDisabled),
                0x0500 => Ok(FpgaError::BadDevice),
                0x0501 => Ok(FpgaError::NotLocked),
                0x0502 => Ok(FpgaError::AlreadyLocked),
                _ => Err(()),
            },
        }
    }
}

impl core::convert::TryFrom<u32> for FpgaError {
    type Error = ();

    fn try_from(v: u32) -> Result<Self, Self::Error> {
        let v: u16 = v.try_into().map_err(|_| ())?;
        Self::try_from(v)
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum DeviceState {
    Unknown = 0,
    Disabled = 1,
    AwaitingBitstream = 2,
    RunningUserDesign = 3,
    Error = 4,
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum BitstreamType {
    Uncompressed = 0,
    Compressed = 1,
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum WriteOp {
    Write = 0,
    // This maps onto a generic Op type in Bluespec which defines Read = 1. The
    // read/write split obviates the need for that operation.
    BitSet = 2,
    BitClear = 3,
}

impl From<WriteOp> for u8 {
    fn from(op: WriteOp) -> Self {
        op as u8
    }
}

impl From<bool> for WriteOp {
    fn from(p: bool) -> Self {
        if p {
            WriteOp::BitSet
        } else {
            WriteOp::BitClear
        }
    }
}

pub struct FpgaLock {
    server: idl::Fpga,
    device_index: u8,
}

impl Deref for FpgaLock {
    type Target = idl::Fpga;

    fn deref(&self) -> &Self::Target {
        &self.server
    }
}

impl Drop for FpgaLock {
    fn drop(&mut self) {
        // We ignore the result of release because, if the server has restarted,
        // we don't need to do anything.
        (*self).release().ok();
    }
}

pub struct Fpga {
    server: idl::Fpga,
    device_index: u8,
}

impl Fpga {
    pub fn new(task_id: userlib::TaskId, device_index: u8) -> Self {
        Self {
            server: idl::Fpga::from(task_id),
            device_index,
        }
    }

    pub fn enabled(&self) -> Result<bool, FpgaError> {
        self.server.device_enabled(self.device_index)
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), FpgaError> {
        self.server.set_device_enabled(self.device_index, enabled)
    }

    pub fn reset(&mut self) -> Result<(), FpgaError> {
        self.server.reset_device(self.device_index)
    }

    pub fn state(&self) -> Result<DeviceState, FpgaError> {
        self.server.device_state(self.device_index)
    }

    pub fn id(&self) -> Result<u32, FpgaError> {
        self.server.device_id(self.device_index)
    }

    pub fn start_bitstream_load(
        &mut self,
        bitstream_type: BitstreamType,
    ) -> Result<Bitstream, FpgaError> {
        let lock = self.lock()?;
        lock.server
            .start_bitstream_load(lock.device_index, bitstream_type)?;
        Ok(Bitstream(lock))
    }

    pub fn lock(&mut self) -> Result<FpgaLock, FpgaError> {
        self.server.lock(self.device_index)?;
        Ok(FpgaLock {
            server: self.server.clone(),
            device_index: self.device_index,
        })
    }
}

pub struct Bitstream(FpgaLock);

impl Bitstream {
    pub fn continue_load(&mut self, data: &[u8]) -> Result<(), FpgaError> {
        (*self.0).continue_bitstream_load(data)
    }

    pub fn finish_load(&mut self) -> Result<(), FpgaError> {
        (*self.0).finish_bitstream_load()
    }
}

pub struct FpgaUserDesign {
    server: idl::Fpga,
    device_index: u8,
}

impl FpgaUserDesign {
    pub fn new(task_id: userlib::TaskId, device_index: u8) -> Self {
        Self {
            server: idl::Fpga::from(task_id),
            device_index,
        }
    }

    pub fn enabled(&self) -> Result<bool, FpgaError> {
        self.server.user_design_enabled(self.device_index)
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), FpgaError> {
        self.server
            .set_user_design_enabled(self.device_index, enabled)
    }

    pub fn reset(&mut self) -> Result<(), FpgaError> {
        self.server.reset_user_design(self.device_index)
    }

    pub fn read<T>(&self, addr: impl Into<u16>) -> Result<T, FpgaError>
    where
        T: AsBytes + Default + FromBytes,
    {
        let mut v = T::default();
        self.server.user_design_read(
            self.device_index,
            addr.into(),
            v.as_bytes_mut(),
        )?;
        Ok(v)
    }

    pub fn write<T>(
        &self,
        op: WriteOp,
        addr: impl Into<u16>,
        value: T,
    ) -> Result<(), FpgaError>
    where
        T: AsBytes + FromBytes,
    {
        self.server.user_design_write(
            self.device_index,
            op,
            addr.into(),
            value.as_bytes(),
        )
    }
}

/// Poll the device state of the FPGA to determine if it is either ready to receive
/// a bitstream or already programmed. The FPGA is reset , resetting the device if needed.
pub fn await_fpga_ready(
    fpga: &mut Fpga,
    sleep_ticks: u64,
) -> Result<DeviceState, FpgaError> {
    loop {
        let state = fpga.state()?;

        match state {
            DeviceState::Disabled => return Err(FpgaError::InvalidState),
            DeviceState::AwaitingBitstream | DeviceState::RunningUserDesign => {
                return Ok(state)
            }
            _ => {
                fpga.reset()?;
            }
        }

        userlib::hl::sleep_for(sleep_ticks);
    }
}

/// Load a bitstream.
pub fn load_bitstream(
    fpga: &mut Fpga,
    data: &[u8],
    bitstream_type: BitstreamType,
    chunk_len: usize,
) -> Result<(), FpgaError> {
    let mut bitstream = fpga.start_bitstream_load(bitstream_type)?;

    for chunk in data.chunks(chunk_len) {
        bitstream.continue_load(chunk)?;
    }

    bitstream.finish_load()
}

pub mod idl {
    use super::{BitstreamType, DeviceState, FpgaError, WriteOp};
    use userlib::*;

    include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
}

#[cfg(feature = "hiffy")]
pub mod hiffy {
    pub use super::idl::Fpga;
}
