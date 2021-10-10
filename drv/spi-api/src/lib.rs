//! Client API for the SPI server
//!
//! # The SPI API model
//!
//! Clients talk to the SPI server task through the `Spi` abstraction. Each task
//! is responsible for a single hardware SPI controller peripheral -- because a
//! given controller can only be running a single transfer at any given time, it
//! is the fundamental unit of concurrency for all attached devices. It
//! serializes all client requests in time, dividing their requests across
//! devices.
//!
//! Speaking of attached devices, each task manages one or more, numbered
//! starting at zero. Electrically, devices have separate `CS_N` chip select
//! lines, and may involve redirecting the SPI signal set SCK/CIPO/COPI to
//! different pads on the SoC. But from the client's perspective, these details
//! are opaque, and devices are simply referenced by index.
//!
//! This means that a program that wishes to use a SPI device needs to be
//! configured in terms of two pieces of information (in addition to whatever
//! else it needs):
//!
//! - The `TaskId` of the server responsible for the right SPI controller, and
//! - The `device_index` of the hardware it seeks.
//!
//! # Controller locks
//!
//! Normally, the controller processes all requests in priority order, asserting
//! and deasserting each device's CS at request boundaries. This isn't always
//! what we want.
//!
//! The SPI API allows clients to *lock* the controller. This causes the
//! controller to only process requests from a single client, for a single
//! device, until the lock is released. The client that locks the controller can
//! also cause CS to be asserted across multiple transactions, which is useful
//! for cases where other signals need to be toggled while CS is asserted.
//!
//! If the task holding the lock crashes or restarts, the lock will be released.
//!
//! Use locks **sparingly.** They have several limitations:
//!
//! - Locks have implications for availability, since they keep other clients
//!   from being able to use devices attached to the same SPI controller,
//!   whether or not data is actively being exchanged. It is possible for a
//!   client to lock the controller and never release it, though the API is
//!   designed to make this difficult.
//!
//! - Lock requests are processed in priority order with all other messages, but
//!   while locked, priority inversion between clients is possible: a lock held
//!   by a lower priority client will prevent higher priority clients from doing
//!   work, even if the lower priority client is itself starved by an
//!   intermediate priority task. For this reason, it's strongly recommended
//!   that all tasks using a given SPI controller be scheduled at the same
//!   priority.
//!
//! - Locks have some composability issues, since they are at the task level. If
//!   multiple libraries in a task are accessing multiple devices that happen to
//!   be on the same SPI controller, and they attempt to lock those devices at
//!   the same time, the second one to try will get an error. It's really only
//!   appropriate to use locks if you can convince yourself that such concurrent
//!   locking can't occur.
//!
//! # Server restarts
//!
//! This design does _not_ assume that SPI requests are idempotent, and so if a
//! client transfer is underway when the server restarts for any reason, the
//! client will receive an error (`ServerRestarted`). Any locks held are lost,
//! and the server will deassert all CS lines. It's up to the client to retry or
//! take other corrective action as needed.

#![no_std]

use core::cell::Cell;
use userlib::*;

/// Raw operation codes. You likely don't need these unless you're writing a
/// server, but, you never know.
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

/// Reasons why SPI client operations may fail.
///
/// There are three classes of errors represented here:
///
/// - **Runtime errors** are things that may reasonably occur in a correct
///   program. You should generally not panic on these.
///
/// - **Configuration errors** suggest that you're not talking to a SPI server,
///   which means the application-level configuration is wrong. These are
///   unlikely to be recoverable and should panic.
///
/// - **Programmer errors** indicate that the calling code has failed to meet
///   the requirements of the operation. These are generally worth panicking on.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u32)]
pub enum SpiError {
    /// Malformed response. The server sent back a response code that doesn't
    /// match our definition of `SpiError`. This is either a server-side bug or
    /// an indication that the task you're talking to is not, in fact, a SPI
    /// server -- so a configuration error.
    BadResponse = 1,

    /// Message sent to the server was the wrong size or otherwise malformed.
    /// This is almost certainly a configuration error if you used the SPI
    /// client code.
    BadArg = 2,

    /// You've provided a single lease and its attributes are wrong for the
    /// operation you've requested. This is a programmer error.
    BadLeaseAttributes = 3,

    /// You've provided two leases for an exchange operation, and the first one
    /// isn't readable. This is a programmer error.
    BadSourceAttributes = 4,

    /// You've provided two leases for an exchange operation, and the second one
    /// isn't writable. This is a programmer error.
    BadSinkAttributes = 5,

    /// You've provided two leases for an exchange operation, and the second one
    /// is shorter than the first, which is currently not supported. This is a
    /// programmer error.
    ShortSinkLength = 6,

    /// Wrong number of leases for operation. This is a programmer error.
    BadLeaseCount = 7,

    /// Transfer size is 0 or exceeds maximum (currently 64kiB). This is a
    /// programmer error.
    BadTransferSize = 8,

    /// Could not transfer byte out of source. This condition is basically only
    /// generated when the client restarts during a transfer, so you're unlikely
    /// to observe this -- if you do, it is likely a server bug.
    ///
    /// TODO this should probably not be in this enum.
    BadSourceByte = 9,

    /// Could not transfer byte into sink. This condition is basically only
    /// generated when the client restarts during a transfer, so you're unlikely
    /// to observe this -- if you do, it is likely a server bug.
    ///
    /// TODO this should probably not be in this enum.
    BadSinkByte = 10,

    /// The server restarted. Clients need to take whatever recovery action is
    /// appropriate for their application.
    ServerRestarted = 11,

    /// You've tried to release a lock, but the server doesn't show you as
    /// holding one. This is a programmer error.
    NothingToRelease = 12,

    /// Attempt to operate device N when there is no device N, or an attempt to
    /// operate on _any other_ device when you've locked the controller to one.
    ///
    /// This is either a programmer or configuration error, with one exception,
    /// which is if you've got two concurrent state machines running in one task
    /// that are both trying to lock devices on a single SPI server. In that
    /// case, it is possible to see this in a correct program, though it's still
    /// rather difficult to recover from.
    BadDevice = 13,
}

impl From<SpiError> for u32 {
    fn from(rc: SpiError) -> Self {
        rc as u32
    }
}

/// A handle to a SPI server task.
///
/// Create one from a `TaskId` using `Spi::from(task_id)`.
///
/// The internal `TaskId` will be updated automatically if we detect a server
/// generation change.
#[derive(Clone, Debug)]
pub struct Spi(Cell<TaskId>);

impl From<TaskId> for Spi {
    fn from(t: TaskId) -> Self {
        Self(Cell::new(t))
    }
}

impl Spi {
    /// Returns a `SpiDevice` that will use this controller with a fixed
    /// `device_index` for your convenience.
    ///
    /// This does _not_ check that `device_index` is valid!
    pub fn device(&self, device_index: u8) -> SpiDevice {
        SpiDevice::new(self.clone(), device_index)
    }

    /// Clock the given device, simultaneously shifting data out of `source` and
    /// corresponding bytes into `sink`. The second slice must be at least as
    /// long as the first.
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    ///
    /// Requirements:
    ///
    /// - `device_index` must be in range for the server.
    /// - If your task is holding a lock on the server, it must match
    ///   `device_index`.
    /// - `source` and `sink` must be less than the current implementation
    ///   transfer limit, which is 64kiB.
    /// - `sink` must be at least one byte long, and at least as long as
    ///   `source`.
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
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    ///
    /// Requirements:
    ///
    /// - `device_index` must be in range for the server.
    /// - If your task is holding a lock on the server, it must match
    ///   `device_index`.
    /// - `source` must not be empty.
    /// - `source` must be less than the current implementation transfer limit,
    ///   which is 64kiB.
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
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    ///
    /// Requirements:
    ///
    /// - `device_index` must be in range for the server.
    /// - If your task is holding a lock on the server, it must match
    ///   `device_index`.
    /// - `sink` must not be empty.
    /// - `sink` must be less than the current implementation transfer limit,
    ///   which is 64kiB.
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
    ///
    /// Requirements:
    ///
    /// - `device_index` must be in range for the server.
    /// - If your task is holding a lock on the server, it must match
    ///   `device_index`. You can use `lock` repeatedly to change `assert_cs`
    ///   but not `device_index`.
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

    /// Utility routine for processing results and updating generations.
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
}

/// Choice of CS state when locking the controller to a device.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CsState {
    NotAsserted = 0,
    Asserted = 1,
}

/// Resource type for managing a controller lock. This is produced by
/// `lock_auto` and releases the lock automatically on `Drop`.
///
/// Note that this borrows the `Spi` handle, which can make it awkward in some
/// cases. The `lock`/`release` explicit APIs may be useful in such cases.
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
///
/// This implements the same operations as `Spi` but with one fewer argument,
/// for your convenience. As a result, every operation on this type (except for
/// `new`) has the following requirements:
///
/// - This `SpiDevice`'s `device_index` must be valid for the server.
/// - If your task is holding a lock on the server, it must match this
///   `SpiDevice`'s `device_index`.
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
    /// corresponding bytes into `sink`. The second slice must be at least as
    /// long as the first.
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    ///
    /// Requirements:
    ///
    /// - `source` and `sink` must be less than the current implementation
    ///   transfer limit, which is 64kiB.
    /// - `sink` must be at least one byte long, and at least as long as
    ///   `source`.
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
    ///
    /// Requirements:
    ///
    /// - `source` must not be empty.
    /// - `source` must be less than the current implementation transfer limit,
    ///   which is 64kiB.
    pub fn write(&self, source: &[u8]) -> Result<(), SpiError> {
        self.server.write(self.device_index, source)
    }

    /// Clock bytes from the device into `dest`.
    ///
    /// If the controller is not locked, this will assert CS before driving the
    /// clock and release it after.
    ///
    /// Requirements:
    ///
    /// - `sink` must not be empty.
    /// - `sink` must be less than the current implementation transfer limit,
    ///   which is 64kiB.
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
    /// Requirements:
    ///
    /// - Your task is not holding a lock on the same server for any other
    ///   device.
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
