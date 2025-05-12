// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Fpga, FpgaBitstream, FpgaUserDesign};
use bitfield::bitfield;
use core::fmt::Debug;
use drv_fpga_api::{DeviceState, FpgaError};
use ringbuf::*;
use userlib::{hl, FromPrimitive, ToPrimitive};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// ECP5 IDCODE values, found in Table B.5, p. 58, Lattice Semi FPGA-TN-02039-2.0.
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    ToPrimitive,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u32)]
pub enum Id {
    Invalid = 0,
    Lfe5u12 = 0x21111043,
    Lfe5u25 = 0x41111043,
    Lfe5u45 = 0x41112043,
    Lfe5u85 = 0x41113043,
    Lfe5um25 = 0x01111043,
    Lfe5um45 = 0x01112043,
    Lfe5um85 = 0x01113043,
    Lfe5um5g25 = 0x81111043,
    Lfe5um5g45 = 0x81112043,
    Lfe5um5g85 = 0x81113043,
}

/// Possible bitstream error codes returned by the device. These values are
/// taken from Table 4.2, p. 10, Lattice Semi FPGA-TN-02039-2.0.
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    ToPrimitive,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u8)]
pub enum BitstreamError {
    InvalidId = 0b001,
    IllegalCommand = 0b010,
    CrcMismatch = 0b011,
    InvalidPreamble = 0b100,
    UserAbort = 0b101,
    DataOverflow = 0b110,
    SramDataOverflow = 0b111,
}

bitfield! {
    /// The device status register, as found in section 4.2, Table 4.2, p. 10,
    /// Lattice Semi FPGA-TN-02039-2.0.
    pub struct Status(u32);
    pub transparent_mode, _: 0;
    pub config_target_selection, _: 3, 1;
    pub jtag_active, _: 4;
    pub pwd_protection, _: 5;
    reserved1, _: 6;
    pub decrypt_enable, _: 7;
    pub done, _: 8;
    pub isc_enabled, _: 9;
    pub write_enabled, _: 10;
    pub read_enabled, _: 11;
    pub busy, _: 12;
    pub fail, _: 13;
    pub fea_otp, _: 14;
    pub decrypt_only, _: 15;
    pub pwd_enabled, _: 16;
    reserved2, _: 19, 17;
    pub encrypt_preamble_detected, _: 20;
    pub standard_preamble_detected, _: 21;
    pub spim_fail1, _: 22;
    pub bse_error_code, _: 25, 23;
    pub execution_error, _: 26;
    pub id_error, _: 27;
    pub invalid_command, _: 28;
    pub sed_error, _: 29;
    pub bypass_mode, _: 30;
    pub flow_through_mode, _: 31;
}

impl Status {
    /// Decode the bitstream error field.
    pub fn bitstream_error(&self) -> Option<BitstreamError> {
        BitstreamError::from_u32(self.bse_error_code())
    }
}

/// Command opcodes which can be sent to the device while in ConfigurationMode.
/// This is a subset of Table 6.4, p. 32, Lattice Semi FPGA-TN-02039-2.0. The
/// table header suggests these are opcodes for SPI commands, but they seem to
/// apply to JTAG as well.
#[derive(
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    Eq,
    PartialEq,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u8)]
pub enum Command {
    Noop = 0xff,
    ReadId = 0xe0,
    ReadUserCode = 0xc0,
    ReadStatus = 0x3c,
    CheckBusy = 0xf0,
    Refresh = 0x79,
    EnableConfigurationMode = 0xc6,
    EnableTransparentConfigurationMode = 0x74,
    DisableConfigurationMode = 0x26,
    Erase = 0x0e,
    BitstreamBurst = 0x7a,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Trace {
    None,
    Disabled,
    Enabled,
    Command(Command),
    ReadId(Id),
    Read32(Command, u32),
    StandardBitstreamDetected,
    EncryptedBitstreamDetected,
    WriteBitstreamChunk,
    BitstreamAccepted,
    BitstreamError(BitstreamError),
    UserDesignResetAsserted,
    UserDesignResetDeasserted,
    Lock,
    Unlock,
}
ringbuf!(Trace, 16, Trace::None);

/// Tiny hack to let ECP5 driver inject additional trace events in this buffer.
pub(crate) fn ecp5_trace(t: Trace) {
    ringbuf_entry!(t);
}

pub const DEVICE_RESET_DURATION: u64 = 25;
pub const USER_DESIGN_RESET_DURATION: u64 = 25;
pub const BUSY_DURATION: u64 = 10;
pub const DONE_DURATION: u64 = 10;

pub trait Ecp5Driver: FpgaUserDesign {
    type Error: Debug;

    /// PROGAM_N interface. This pin acts as a device reset and when asserted
    /// low force to (re)start the bitstream loading process.
    ///
    /// See FPGA-TN-02039-2.0, 4.5.2 for details.
    fn program_n(&self) -> Result<bool, Self::Error>;
    fn set_program_n(&self, asserted: bool) -> Result<(), Self::Error>;

    /// INIT_N interface. This pin can be driven after reset/power up to keep
    /// the device from entering Configuration state. As input it signals
    /// Initialization complete or an error occured during bitstream loading.
    ///
    /// See FPGA-TN-02039-2.0, 4.5.3 for details.
    fn init_n(&self) -> Result<bool, Self::Error>;
    fn set_init_n(&self, asserted: bool) -> Result<(), Self::Error>;

    /// DONE interface. This pin signals the device is in User Mode. Asserting
    /// the pin keeps the device from entering User Mode after Configuration.
    ///
    /// See FPGA-TN-02039-2.0, 4.5.4 for details.
    fn done(&self) -> Result<bool, Self::Error>;
    fn set_done(&self, asserted: bool) -> Result<(), Self::Error>;

    /// A generic interface to send commands and read/write data from a
    /// configuration port. This interface is intended to be somewhat transport
    /// agnostic so either SPI or JTAG could be implemented if desired.
    fn configuration_read(&self, data: &mut [u8]) -> Result<(), Self::Error>;
    fn configuration_write(&self, data: &[u8]) -> Result<(), Self::Error>;
    fn configuration_write_command(
        &self,
        c: Command,
    ) -> Result<(), Self::Error>;

    /// The configuration interface may exist on a shared medium such as SPI.
    /// The following primitives allow the upper half of the driver to issue
    /// atomic commands.
    ///
    /// If no lock control of the medium is needed these can be implemented as
    /// no-op.
    fn configuration_lock(&self) -> Result<(), Self::Error>;
    fn configuration_release(&self) -> Result<(), Self::Error>;

    /// User design reset.
    fn user_design_reset_n(&self) -> Result<bool, Self::Error>;
    fn set_user_design_reset_n(
        &self,
        asserted: bool,
    ) -> Result<(), Self::Error>;

    /// Returns the reset duration in ms for the user design.
    fn user_design_reset_duration(&self) -> u64;

    /*
    /// Read/write the user design.
    fn user_design_read(&self, data: &mut [u8]) -> Result<(), Self::Error>;
    fn user_design_write(&self, data: &[u8]) -> Result<(), Self::Error>;

    /// Lock the user design for multiple uninterrupted operations.
    ///
    /// Note: the semantics of this are not well defined and need work.
    fn user_design_lock(&self) -> Result<(), Self::Error>;

    /// Release the lock on the user design held previously.
    fn user_design_release(&self) -> Result<(), Self::Error>;
    */
}

/// Newtype wrapping an Impl reference, allowing the remaining traits to be
/// implemented.
pub struct Ecp5<Driver: Ecp5Driver> {
    driver: Driver,
}

/// Additional ECP5 methods to help implement high level behavior.
impl<Driver: Ecp5Driver> Ecp5<Driver> {
    /// Allocate a new `Ecp5` device from the given implmenetation.
    pub fn new(driver: Driver) -> Self {
        Self { driver }
    }

    /// Lock (resources in) the driver, returning a lock object which
    /// automatically releases the driver when dropped.
    fn lock(&self) -> Result<Ecp5Lock<'_, Driver>, Driver::Error> {
        self.driver.configuration_lock()?;
        ringbuf_entry!(Trace::Lock);
        Ok(Ecp5Lock(&self.driver))
    }

    /// Send a command to the device which does not return or require additional
    /// data. FPGA-TN-02039-2.0, 6.2.5 refers to this as a Class C command.
    pub fn send_command(&self, c: Command) -> Result<(), Driver::Error> {
        self.driver.configuration_write_command(c)?;
        ringbuf_entry!(Trace::Command(c));
        Ok(())
    }

    /// Send a command and read back a number of bytes given by the type T. Note
    /// that data is always returned in big endian order.
    pub fn read<T: IntoBytes + FromBytes>(
        &self,
        c: Command,
    ) -> Result<T, Driver::Error> {
        let mut buf = T::new_zeroed();

        // Release of the lock happens implicit as we leave this function.
        let _lock = self.lock()?;

        self.driver.configuration_write_command(c)?;
        self.driver.configuration_read(buf.as_mut_bytes())?;

        Ok(buf)
    }

    /// Send a `Command` and read back two bytes of data.
    pub fn read16(&self, c: Command) -> Result<u16, Driver::Error> {
        Ok(u16::from_be(self.read(c)?))
    }

    /// Send a `Command` and read back four bytes of data.
    pub fn read32(&self, c: Command) -> Result<u32, Driver::Error> {
        let v = u32::from_be(self.read(c)?);
        ringbuf_entry!(Trace::Read32(c, v));
        Ok(v)
    }

    /// Read the Status register
    pub fn status(&self) -> Result<Status, Driver::Error> {
        self.read32(Command::ReadStatus).map(Status)
    }

    /// Enable ConfigurationMode, allowing access to certain configuration
    /// command and the bitstream loading process.
    pub fn enable_configuration_mode(&self) -> Result<(), Driver::Error> {
        self.send_command(Command::EnableConfigurationMode)
    }

    /// Leave ConfigurationMode, disabling access to certaion commands. In
    /// addition, if a bitstream was loaded, this will transition the device to
    /// UserMode.
    pub fn disable_configuration_mode(&self) -> Result<(), Driver::Error> {
        self.send_command(Command::DisableConfigurationMode)
    }

    /// Wait for the device to finish an operation in progress, sleeping for the
    /// given duration between polling events.
    pub fn await_not_busy(
        &self,
        sleep_ticks: u64,
    ) -> Result<Status, Driver::Error> {
        let mut status = self.status()?;

        while status.busy() {
            hl::sleep_for(sleep_ticks);
            status = self.status()?;
        }

        Ok(status)
    }

    /// Wait for the DONE flag to go high.
    pub fn await_done(&self, sleep_ticks: u64) -> Result<(), Driver::Error> {
        while !self.driver.done()? {
            hl::sleep_for(sleep_ticks);
        }
        Ok(())
    }
}

/// A lock type which implements Drop, allowing for automatic resource
/// management in the lower driver.
pub struct Ecp5Lock<'a, Driver: Ecp5Driver>(&'a Driver);

impl<Driver: Ecp5Driver> Drop for Ecp5Lock<'_, Driver> {
    fn drop(&mut self) {
        ringbuf_entry!(Trace::Unlock);
        self.0.configuration_release().unwrap();
    }
}

/// Implement the FPGA trait for ECP5.
impl<'a, Driver: 'a + Ecp5Driver> Fpga<'a> for Ecp5<Driver>
where
    FpgaError: From<<Driver as Ecp5Driver>::Error>,
{
    type Bitstream = Ecp5Bitstream<'a, Driver>;

    fn device_enabled(&self) -> Result<bool, FpgaError> {
        Ok(self.driver.program_n()?)
    }

    fn set_device_enabled(&self, enabled: bool) -> Result<(), FpgaError> {
        self.driver.set_program_n(enabled)?;
        ringbuf_entry!(if enabled {
            Trace::Enabled
        } else {
            Trace::Disabled
        });
        Ok(())
    }

    fn reset_device(&self) -> Result<(), FpgaError> {
        self.set_device_enabled(false)?;
        hl::sleep_for(DEVICE_RESET_DURATION);
        self.set_device_enabled(true)?;
        Ok(())
    }

    fn device_state(&self) -> Result<DeviceState, FpgaError> {
        if !self.driver.program_n()? {
            Ok(DeviceState::Disabled)
        } else if self.driver.done()? {
            Ok(DeviceState::RunningUserDesign)
        } else if self.driver.init_n()? {
            Ok(DeviceState::AwaitingBitstream)
        } else {
            Ok(DeviceState::Error)
        }
    }

    fn device_id(&self) -> Result<u32, FpgaError> {
        let v = self.read32(Command::ReadId)?;
        let id = Id::from_u32(v).ok_or(FpgaError::InvalidValue)?;
        ringbuf_entry!(Trace::ReadId(id));
        Ok(v)
    }

    fn start_bitstream_load(&'a self) -> Result<Self::Bitstream, FpgaError> {
        // Put device in configuration mode if required.
        if !self.status()?.write_enabled() {
            self.enable_configuration_mode()?;

            if !self.status()?.write_enabled() {
                return Err(FpgaError::InvalidState);
            }
        }

        // Assert the design reset.
        self.driver.set_user_design_reset_n(false)?; // Negative asserted.

        // Lock the lower part of the driver in anticipation of writing the
        // bitstream.
        let lock = self.lock()?;

        // Use the Impl to write the command and leave the device locked for the
        // byte stream to follow.
        self.driver
            .configuration_write_command(Command::BitstreamBurst)?;
        ringbuf_entry!(Trace::Command(Command::BitstreamBurst));

        Ok(Ecp5Bitstream {
            device: self,
            lock: Some(lock),
        })
    }
}

/// An Ecp5Bitstream type.
pub struct Ecp5Bitstream<'a, Driver: Ecp5Driver> {
    device: &'a Ecp5<Driver>,
    lock: Option<Ecp5Lock<'a, Driver>>,
}

impl<Driver: Ecp5Driver> Drop for Ecp5Bitstream<'_, Driver> {
    fn drop(&mut self) {
        self.lock = None
    }
}

impl<Driver: Ecp5Driver> FpgaBitstream for Ecp5Bitstream<'_, Driver>
where
    FpgaError: From<<Driver as Ecp5Driver>::Error>,
{
    fn continue_load(&mut self, buf: &[u8]) -> Result<(), FpgaError> {
        if self.lock.is_none() {
            return Err(FpgaError::InvalidState);
        }

        self.lock.as_ref().unwrap().0.configuration_write(buf)?;

        ringbuf_entry!(Trace::WriteBitstreamChunk);
        Ok(())
    }

    fn finish_load(&mut self) -> Result<(), FpgaError> {
        if self.lock.is_none() {
            return Err(FpgaError::InvalidState);
        }

        // Release the locked resources in the driver.
        self.lock = None;

        // Perform climb-out checklist; determine if the bitstream was accepted
        // and the device is ready for wake up.
        let status = self.device.await_not_busy(BUSY_DURATION)?;

        if status.encrypt_preamble_detected() {
            ringbuf_entry!(Trace::EncryptedBitstreamDetected);
        }
        if status.standard_preamble_detected() {
            ringbuf_entry!(Trace::StandardBitstreamDetected);
        }

        if let Some(error) = status.bitstream_error() {
            // Log and bail. This leaves the device in configuration mode (and
            // the SPI port enabled), allowing the caller to issue a Refresh
            // command and try again if so desired.
            ringbuf_entry!(Trace::BitstreamError(error));
            return Err(FpgaError::BitstreamError(error as u8));
        }

        ringbuf_entry!(Trace::BitstreamAccepted);

        // Return to user mode, initiating the control sequence which will start
        // the fabric. Completion of this transition is externally observable
        // with the DONE pin going high.
        //
        // Unless the port is set to remain enabled through the FAE bits it will
        // be disabled at this point, i.e. performing a read of the ID or Status
        // registers will result in a PortDisabled error.
        self.device.disable_configuration_mode()?;

        self.device.await_done(DONE_DURATION)?;

        hl::sleep_for(self.device.driver.user_design_reset_duration());
        self.device.driver.set_user_design_reset_n(true)?; // Negative asserted.

        Ok(())
    }
}

/// Implement the FpgaUserDesign trait for ECP5.
impl<Driver: Ecp5Driver> FpgaUserDesign for Ecp5<Driver> {
    fn user_design_enabled(&self) -> Result<bool, FpgaError> {
        self.driver.user_design_enabled()
    }

    fn set_user_design_enabled(&self, enabled: bool) -> Result<(), FpgaError> {
        self.driver.set_user_design_enabled(enabled)
    }

    fn reset_user_design(&self) -> Result<(), FpgaError> {
        self.driver.reset_user_design()
    }

    fn user_design_read(&self, data: &mut [u8]) -> Result<(), FpgaError> {
        self.driver.user_design_read(data)
    }

    fn user_design_write(&self, data: &[u8]) -> Result<(), FpgaError> {
        self.driver.user_design_write(data)
    }

    fn user_design_lock(&self) -> Result<(), FpgaError> {
        self.driver.user_design_lock()
    }

    fn user_design_release(&self) -> Result<(), FpgaError> {
        self.driver.user_design_release()
    }
}
