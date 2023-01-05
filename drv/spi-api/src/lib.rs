// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the SPI server

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
#[repr(u32)]
pub enum SpiError {
    /// Transfer size is 0 or exceeds maximum
    BadTransferSize = 1,

    /// Server restarted
    #[idol(server_death)]
    ServerRestarted = 2,

    /// Release without successful Lock
    NothingToRelease = 3,

    /// Attempt to operate device N when there is no device N, or an attempt to
    /// operate on _any other_ device when you've locked the controller to one.
    ///
    /// This is almost certainly a programming error on the client side.
    BadDevice = 4,
}

#[derive(
    Copy, Clone, Debug, Eq, PartialEq, zerocopy::AsBytes, FromPrimitive,
)]
#[repr(u8)]
pub enum CsState {
    NotAsserted = 0,
    Asserted = 1,
}

impl Spi {
    /// Variant of `lock` that returns a resource management object that, when
    /// dropped, will issue `release`. This makes it much easier to do fallible
    /// operations while locked.
    ///
    /// Otherwise, the rules are the same as for `lock`.
    pub fn lock_auto(
        &self,
        device_index: u8,
        assert_cs: CsState,
    ) -> Result<ControllerLock<'_>, SpiError> {
        self.lock(device_index, assert_cs)?;
        Ok(ControllerLock(self))
    }

    /// Returns a `SpiDevice` that will use this controller with a fixed
    /// `device_index` for your convenience.
    ///
    /// This does _not_ check that `device_index` is valid!
    pub fn device(&self, device_index: u8) -> SpiDevice {
        SpiDevice::new(self.clone(), device_index)
    }
}

pub struct ControllerLock<'a>(&'a Spi);

impl Drop for ControllerLock<'_> {
    fn drop(&mut self) {
        // We ignore the result of release because, if the server has restarted,
        // we don't need to do anything.
        self.0.release().ok();
    }
}

/// Wraps a `Spi`, pairing it with a `device_index` that will automatically be
/// sent with all operations.
pub struct SpiDevice {
    server: Spi,
    device_index: u8,
}

impl SpiDevice {
    /// Creates a wrapper for `(server, device_index)`. Note that this does
    /// _not_ check that `device_index` is valid for `server`. If it isn't, all
    /// operations on this `SpiDevice` are going to give you `BadDevice`.
    pub fn new(server: Spi, device_index: u8) -> Self {
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
    pub fn lock(&self, assert_cs: CsState) -> Result<(), SpiError> {
        self.server.lock(self.device_index, assert_cs)
    }

    /// Variant of `lock` that returns a resource management object that, when
    /// dropped, will issue `release`. This makes it much easier to do fallible
    /// operations while locked.
    ///
    /// Otherwise, the rules are the same as for `lock`.
    pub fn lock_auto(
        &self,
        assert_cs: CsState,
    ) -> Result<ControllerLock<'_>, SpiError> {
        self.server.lock_auto(self.device_index, assert_cs)
    }

    /// Releases a previous lock on the SPI controller (by your task).
    ///
    /// This will also deassert CS, if you had overridden it.
    ///
    /// If you call this without `lock` having succeeded, you will get
    /// `SpiError::NothingToRelease`.
    pub fn release(&self) -> Result<(), SpiError> {
        self.server.release()
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
