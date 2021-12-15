// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the SPI server

#![no_std]

use core::cell::Cell;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq)]
pub enum Operation {
    Read = 0b01,
    Write = 0b10,
    Exchange = 0b11,
    Lock = 0b100,
    Release = 0b101,
}

impl Operation {
    pub fn is_read(self) -> bool {
        self == Self::Read || self == Self::Exchange
    }

    pub fn is_write(self) -> bool {
        self == Self::Write || self == Self::Exchange
    }
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u32)]
pub enum SpiError {
    /// Malformed response
    BadResponse = 1,

    /// Bad argument
    BadArg = 2,

    /// Bad lease argument
    BadLeaseArg = 3,

    /// Bad lease attributes
    BadLeaseAttributes = 4,

    /// Bad source lease
    BadSource = 5,

    /// Bad source lease attibutes
    BadSourceAttributes = 6,

    /// Bad Sink lease
    BadSink = 7,

    /// Bad Sink lease attributes
    BadSinkAttributes = 8,

    /// Short sink length
    ShortSinkLength = 9,

    /// Bad lease count
    BadLeaseCount = 10,

    /// Transfer size is 0 or exceeds maximum
    BadTransferSize = 11,

    /// Could not transfer byte out of source
    BadSourceByte = 12,

    /// Could not transfer byte into sink
    BadSinkByte = 13,

    /// Server restarted
    ServerRestarted = 14,

    /// Release without successful Lock
    NothingToRelease = 15,

    /// Attempt to operate device N when there is no device N, or an attempt to
    /// operate on _any other_ device when you've locked the controller to one.
    ///
    /// This is almost certainly a programming error on the client side.
    BadDevice = 16,
}

impl From<SpiError> for u32 {
    fn from(rc: SpiError) -> Self {
        rc as u32
    }
}

#[derive(Clone, Debug)]
pub struct Spi(Cell<TaskId>);

impl From<TaskId> for Spi {
    fn from(t: TaskId) -> Self {
        Self(Cell::new(t))
    }
}

impl Spi {
    fn result(&self, task: TaskId, code: u32) -> Result<(), SpiError> {
        if code != 0 {
            //
            // If we have an error code, check to see if it denotes a dearly
            // departed task; if it does, in addition to returning a specific
            // error code, we will set our task to be the new task as a courtesy.
            //
            if let Some(g) = abi::extract_new_generation(code) {
                self.0.set(TaskId::for_index_and_gen(task.index(), g));
                Err(SpiError::ServerRestarted)
            } else {
                Err(SpiError::from_u32(code).ok_or(SpiError::BadResponse)?)
            }
        } else {
            Ok(())
        }
    }

    /// Returns a `SpiDevice` that will use this controller with a fixed
    /// `device_index` for your convenience.
    ///
    /// This does _not_ check that `device_index` is valid!
    pub fn device(&self, device_index: u8) -> SpiDevice {
        SpiDevice::new(self.clone(), device_index)
    }

    /// Clock the given device, simultaneously shifting data out of `source` and
    /// corresponding bytes into `sink`. (The two slices must be the same
    /// length.)
    ///
    /// `device_index` must be in range for the server.
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    pub fn exchange(
        &self,
        device_index: u8,
        source: &[u8],
        sink: &mut [u8],
    ) -> Result<(), SpiError> {
        let task = self.0.get();

        let (code, _) = sys_send(
            task,
            Operation::Exchange as u16,
            &[device_index],
            &mut [],
            &[Lease::from(source), Lease::from(sink)],
        );

        self.result(task, code)
    }

    /// Clock bytes from `source` into the given device.
    ///
    /// `device_index` must be in range for the server.
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    pub fn write(
        &self,
        device_index: u8,
        source: &[u8],
    ) -> Result<(), SpiError> {
        let task = self.0.get();

        let (code, _) = sys_send(
            task,
            Operation::Write as u16,
            &[device_index],
            &mut [],
            &[Lease::from(source)],
        );

        self.result(task, code)
    }

    /// Clock bytes from the given device into `dest`.
    ///
    /// `device_index` must be in range for the server.
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    pub fn read(
        &self,
        device_index: u8,
        dest: &mut [u8],
    ) -> Result<(), SpiError> {
        let task = self.0.get();

        let (code, _) = sys_send(
            task,
            Operation::Read as u16,
            &[device_index],
            &mut [],
            &[Lease::from(dest)],
        );

        self.result(task, code)
    }

    /// Locks the SPI controller in communication between your task and the
    /// given device.
    ///
    /// If the server receives this message, it means no other task had locked
    /// it. It will respond by only listening to messages from your task until
    /// you send `release` or crash.
    ///
    /// During this time, the server will refuse any attempts to manipulate a
    /// device other than the `device_index` given here.
    ///
    /// `assert_cs` can be used to force CS into the asserted (low) state, or
    /// keep it deasserted. If you choose to assert it, then SPI transactions
    /// via `read`/`write`/`exchange` will leave it asserted rather than
    /// toggling it. You can call `lock` while the SPI controller is locked (by
    /// you) to alter CS state, either to toggle it on its own, or to enable
    /// per-transaction CS control again. However, if you call `lock` more than
    /// once, you must keep the same `device_index`. To change devices, use
    /// `release` first.
    pub fn lock(
        &self,
        device_index: u8,
        assert_cs: CsState,
    ) -> Result<(), SpiError> {
        let task = self.0.get();

        let (code, _) = sys_send(
            task,
            Operation::Lock as u16,
            &[device_index, assert_cs as u8],
            &mut [],
            &[],
        );

        self.result(task, code)
    }

    /// Variant of `lock` that returns a resource management object that, when
    /// dropped, will issue `release`. This makes it much easier to do fallible
    /// operations while locked.
    ///
    /// Otherwise, the rules are the same as for `lock`.
    pub fn lock_auto(
        &self,
        device_index: u8,
        assert_cs: CsState,
    ) -> Result<ControllerLock, SpiError> {
        self.lock(device_index, assert_cs)?;
        Ok(ControllerLock(self))
    }

    /// Releases a previous lock on the SPI controller (by your task).
    ///
    /// This will also deassert CS, if you had overridden it.
    ///
    /// If you call this without `lock` having succeeded, you will get
    /// `SpiError::NothingToRelease`.
    pub fn release(&self) -> Result<(), SpiError> {
        let task = self.0.get();

        let (code, _) =
            sys_send(task, Operation::Release as u16, &[], &mut [], &[]);

        self.result(task, code)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CsState {
    NotAsserted = 0,
    Asserted = 1,
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
    ) -> Result<ControllerLock, SpiError> {
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
