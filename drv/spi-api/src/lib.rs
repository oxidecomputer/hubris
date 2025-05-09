// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the SPI server

#![no_std]

// For Idol-generated code that fully qualifies error type names.
use crate as drv_spi_api;
use derive_idol_err::IdolError;
use gateway_messages::SpiError as GwSpiError;
use hubpack::SerializedSize;
use serde::{Deserialize, Serialize};
use userlib::*;

#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    IdolError,
    SerializedSize,
    Serialize,
    Deserialize,
    counters::Count,
)]
#[repr(u32)]
pub enum SpiError {
    /// Transfer size is 0 or exceeds maximum
    BadTransferSize = 1,

    /// Server restarted
    #[idol(server_death)]
    TaskRestarted = 4,
}

impl From<idol_runtime::ServerDeath> for SpiError {
    fn from(_: idol_runtime::ServerDeath) -> Self {
        SpiError::TaskRestarted
    }
}

impl From<SpiError> for GwSpiError {
    fn from(value: SpiError) -> Self {
        match value {
            SpiError::BadTransferSize => Self::BadTransferSize,
            SpiError::TaskRestarted => Self::TaskRestarted,
        }
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    Eq,
    PartialEq,
    zerocopy::IntoBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
    FromPrimitive,
)]
#[repr(u8)]
pub enum CsState {
    NotAsserted = 0,
    Asserted = 1,
}

////////////////////////////////////////////////////////////////////////////////

pub struct ControllerLock<'a, S: SpiServer>(&'a S);

impl<S: SpiServer> Drop for ControllerLock<'_, S> {
    fn drop(&mut self) {
        // We ignore the result of release because, if the server has restarted,
        // we don't need to do anything.
        let _ = self.0.release();
    }
}

////////////////////////////////////////////////////////////////////////////////

pub trait SpiServer {
    fn exchange(
        &self,
        device_index: u8,
        src: &[u8],
        dest: &mut [u8],
    ) -> Result<(), SpiError>;

    fn write(&self, device_index: u8, src: &[u8]) -> Result<(), SpiError>;

    fn read(&self, device_index: u8, dest: &mut [u8]) -> Result<(), SpiError>;

    /// Variant of `lock` that returns a resource management object that, when
    /// dropped, will issue `release`. This makes it much easier to do fallible
    /// operations while locked.
    ///
    /// Otherwise, the rules are the same as for `lock`.
    fn lock_auto(
        &self,
        device_index: u8,
        assert_cs: CsState,
    ) -> Result<ControllerLock<'_, Self>, idol_runtime::ServerDeath>
    where
        Self: Sized,
    {
        self.lock(device_index, assert_cs)?;
        Ok(ControllerLock(self))
    }

    /// Returns a `SpiDevice` that will use this controller with a fixed
    /// `device_index` for your convenience.
    ///
    /// This does _not_ check that `device_index` is valid!
    fn device(&self, device_index: u8) -> SpiDevice<Self>
    where
        Self: Sized + Clone,
    {
        SpiDevice::new(self.clone(), device_index)
    }

    fn lock(
        &self,
        device_index: u8,
        cs_state: CsState,
    ) -> Result<(), idol_runtime::ServerDeath>;

    fn release(&self) -> Result<(), idol_runtime::ServerDeath>;
}

impl SpiServer for Spi {
    fn exchange(
        &self,
        device_index: u8,
        src: &[u8],
        dest: &mut [u8],
    ) -> Result<(), SpiError> {
        Spi::exchange(self, device_index, src, dest)
    }
    fn write(&self, device_index: u8, src: &[u8]) -> Result<(), SpiError> {
        Spi::write(self, device_index, src)
    }

    fn read(&self, device_index: u8, dest: &mut [u8]) -> Result<(), SpiError> {
        Spi::read(self, device_index, dest)
    }

    fn lock(
        &self,
        device_index: u8,
        cs_state: CsState,
    ) -> Result<(), idol_runtime::ServerDeath> {
        Spi::lock(self, device_index, cs_state)
    }

    fn release(&self) -> Result<(), idol_runtime::ServerDeath> {
        Spi::release(self)
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Wraps a `Spi`, pairing it with a `device_index` that will automatically be
/// sent with all operations.
pub struct SpiDevice<S> {
    server: S,
    device_index: u8,
}

impl<S: SpiServer> SpiDevice<S> {
    /// Creates a wrapper for `(server, device_index)`. Note that this does
    /// _not_ check that `device_index` is valid for `server`. If it isn't, all
    /// operations on this `SpiDevice` are going to give you `BadDevice`.
    pub fn new(server: S, device_index: u8) -> Self {
        Self {
            server,
            device_index,
        }
    }

    /// Clock the device, simultaneously shifting data out of `source` and
    /// corresponding bytes into `sink`. (The two slices must be the same
    /// length.)
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    pub fn exchange(
        &self,
        source: &[u8],
        sink: &mut [u8],
    ) -> Result<(), SpiError> {
        self.server.exchange(self.device_index, source, sink)
    }

    /// Clock bytes from `source` into the device.
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    pub fn write(&self, source: &[u8]) -> Result<(), SpiError> {
        self.server.write(self.device_index, source)
    }

    /// Clock bytes from the device into `dest`.
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    pub fn read(&self, dest: &mut [u8]) -> Result<(), SpiError> {
        self.server.read(self.device_index, dest)
    }

    /// Locks the SPI controller in communication between your task and the
    /// device.
    ///
    /// If the server receives this message, it means no other task had locked
    /// it. It will respond by only listening to messages from your task until
    /// you send `release` or crash.
    ///
    /// During this time, the server will refuse any attempts to manipulate a
    /// device other than the `device_index` of this device.
    ///
    /// `assert_cs` can be used to force CS into the asserted (low) state, or
    /// keep it deasserted. If you choose to assert it, then SPI transactions
    /// via `read`/`write`/`exchange` will leave it asserted rather than
    /// toggling it. You can call `lock` while the SPI controller is locked (by
    /// you) to alter CS state, either to toggle it on its own, or to enable
    /// per-transaction CS control again.
    ///
    /// If your task tries to lock two different `SpiDevice`s at once, the
    /// second one to attempt will get `BadDevice`.
    pub fn lock(
        &self,
        assert_cs: CsState,
    ) -> Result<(), idol_runtime::ServerDeath> {
        self.server.lock(self.device_index, assert_cs)
    }

    /// Releases a previous lock on the SPI controller (by your task).
    ///
    /// This will also deassert CS, if you had overridden it.
    pub fn release(&self) -> Result<(), idol_runtime::ServerDeath> {
        self.server.release()
    }

    /// Variant of `lock` that returns a resource management object that, when
    /// dropped, will issue `release`. This makes it much easier to do fallible
    /// operations while locked.
    ///
    /// Otherwise, the rules are the same as for `lock`.
    pub fn lock_auto(
        &self,
        assert_cs: CsState,
    ) -> Result<ControllerLock<'_, S>, idol_runtime::ServerDeath> {
        self.server.lock_auto(self.device_index, assert_cs)
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
include!(concat!(env!("OUT_DIR"), "/spi_devices.rs"));
