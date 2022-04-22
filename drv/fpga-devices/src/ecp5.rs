// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::Fpga;
use bitfield::bitfield;
use drv_fpga_api::{DeviceState, FpgaError};
use ringbuf::*;
use userlib::*;
use zerocopy::{AsBytes, FromBytes};

/// ECP5 IDCODE values.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum Id {
    Invalid,
    Lfe5u12,
    Lfe5u25,
    Lfe5u45,
    Lfe5u85,
    Lfe5um25,
    Lfe5um45,
    Lfe5um85,
    Lfe5um5g25,
    Lfe5um5g45,
    Lfe5um5g85,
}

impl From<u32> for Id {
    fn from(x: u32) -> Self {
        match x {
            0x21111043 => Id::Lfe5u12,
            0x41111043 => Id::Lfe5u25,
            0x41112043 => Id::Lfe5u45,
            0x41113043 => Id::Lfe5u85,
            0x01111043 => Id::Lfe5um25,
            0x01112043 => Id::Lfe5um45,
            0x01113043 => Id::Lfe5um85,
            0x81111043 => Id::Lfe5um5g25,
            0x81112043 => Id::Lfe5um5g45,
            0x81113043 => Id::Lfe5um5g85,
            _ => Id::Invalid,
        }
    }
}

impl From<Id> for u32 {
    fn from(id: Id) -> Self {
        match id {
            Id::Lfe5u12 => 0x21111043,
            Id::Lfe5u25 => 0x41111043,
            Id::Lfe5u45 => 0x41112043,
            Id::Lfe5u85 => 0x41113043,
            Id::Lfe5um25 => 0x01111043,
            Id::Lfe5um45 => 0x01112043,
            Id::Lfe5um85 => 0x01113043,
            Id::Lfe5um5g25 => 0x81111043,
            Id::Lfe5um5g45 => 0x81112043,
            Id::Lfe5um5g85 => 0x81113043,
            Id::Invalid => 0,
        }
    }
}

/// Possible bitstream error codes returned by the device.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
#[repr(u8)]
pub enum BitstreamError {
    None = 0b000,
    InvalidId = 0b001,
    IllegalCommand = 0b010,
    CrcMismatch = 0b011,
    InvalidPreamble = 0b100,
    UserAbort = 0b101,
    DataOverflow = 0b110,
    SramDataOverflow = 0b111,
}

bitfield! {
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
    pub fn bitstream_error(&self) -> BitstreamError {
        match self.bse_error_code() {
            0b001 => BitstreamError::InvalidId,
            0b010 => BitstreamError::IllegalCommand,
            0b011 => BitstreamError::CrcMismatch,
            0b100 => BitstreamError::InvalidPreamble,
            0b101 => BitstreamError::UserAbort,
            0b110 => BitstreamError::DataOverflow,
            0b111 => BitstreamError::SramDataOverflow,
            _ => BitstreamError::None,
        }
    }
}

/// Command opcodes which can be sent to the device while in ConfigurationMode.
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq, AsBytes)]
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

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Disabled,
    Enabled,
    Command(Command),
    ReadId(u32, Id),
    ReadUserCode(u32),
    ReadStatus(u32),
    Read32(Command, u32),
    StandardBitstreamDetected,
    EncryptedBitstreamDetected,
    WriteBitstreamChunk,
    BitstreamError(BitstreamError),
    ApplicationResetAsserted,
    ApplicationResetDeasserted,
}
ringbuf!(Trace, 16, Trace::None);

pub trait Ecp5Impl {
    type Error;

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

    /// Application Reset.
    fn application_reset_n(&self) -> Result<bool, Self::Error>;
    fn set_application_reset_n(
        &self,
        asserted: bool,
    ) -> Result<(), Self::Error>;

    /// A generic interface to send commands and read/write data from a
    /// configuration port. This interface is intended to be somewhat transport
    /// agnostic so either SPI or JTAG could be implemented if desired.
    fn write_command(&self, c: Command) -> Result<(), Self::Error>;
    fn read(&self, buf: &mut [u8]) -> Result<(), Self::Error>;
    fn write(&self, buf: &[u8]) -> Result<(), Self::Error>;

    /// The command interface may exist on a shared medium such as SPI. The
    /// following primitives allow the upper half of the driver to issue atomic
    /// commands.
    ///
    /// If no lock control of the medium is needed these can be implemented as
    /// no-op.
    fn lock(&self) -> Result<(), Self::Error>;
    fn release(&self) -> Result<(), Self::Error>;
}

/// Newtype wrapping an Impl, allowing the remaining traits to be implemented.
pub struct Ecp5<ImplT: Ecp5Impl>(ImplT);

/// Additional ECP5 methods to help implement high level behavior.
impl<ImplT: Ecp5Impl> Ecp5<ImplT> {
    /// Send a command to the device which does not return or require additional
    /// data. FPGA-TN-02039-2.0, 6.2.5 refers to this as a Class C command.
    pub fn send_command(&self, c: Command) -> Result<(), ImplT::Error> {
        self.0.lock()?;
        self.0.write_command(c)?;
        self.0.release()?;
        ringbuf_entry!(Trace::Command(c));
        Ok(())
    }

    /// Send a command and read back a number of bytes given by the type T. Note
    /// that data is always returned in big endian order.
    pub fn read<T: Default + AsBytes + FromBytes>(
        &self,
        c: Command,
    ) -> Result<T, ImplT::Error> {
        let mut buf = T::default();

        self.0.lock()?;
        self.0.write_command(c)?;
        self.0.read(buf.as_bytes_mut())?;
        self.0.release()?;

        Ok(buf)
    }

    /// Send a `Command` and read back two bytes of data.
    pub fn read16(&self, c: Command) -> Result<u16, ImplT::Error> {
        Ok(u16::from_be(self.read(c)?))
    }

    /// Send a `Command` and read back four bytes of data.
    pub fn read32(&self, c: Command) -> Result<u32, ImplT::Error> {
        let v = u32::from_be(self.read(c)?);

        match c {
            Command::ReadId => {
                ringbuf_entry!(Trace::ReadId(v, Id::from(v)))
            }
            Command::ReadStatus => ringbuf_entry!(Trace::ReadStatus(v)),
            Command::ReadUserCode => ringbuf_entry!(Trace::ReadUserCode(v)),
            _ => ringbuf_entry!(Trace::Read32(c, v)),
        }

        Ok(v)
    }

    /// Read the Status register
    pub fn status(&self) -> Result<Status, ImplT::Error> {
        Ok(Status(self.read32(Command::ReadStatus)?))
    }

    /// Enable ConfigurationMode, allowing access to certain configuration
    /// command and the bitstream loading process.
    pub fn enable_configuration_mode(&self) -> Result<(), ImplT::Error> {
        self.send_command(Command::EnableConfigurationMode)
    }

    /// Leave ConfigurationMode, disabling access to certaion commands. In
    /// addition, if a bitstream was loaded, this will transition the device to
    /// UserMode.
    pub fn disable_configuration_mode(&self) -> Result<(), ImplT::Error> {
        self.send_command(Command::DisableConfigurationMode)
    }

    /// Wait for the device to finish an operation in progress, sleeping for the
    /// given duration between polling events.
    pub fn await_not_busy(&self, sleep_ticks: u64) -> Result<(), ImplT::Error> {
        while self.status()?.busy() {
            hl::sleep_for(sleep_ticks);
        }
        Ok(())
    }

    /// Wait for the DONE flag to go high.
    pub fn await_done(&self, sleep_ticks: u64) -> Result<(), ImplT::Error> {
        while !self.status()?.done() {
            hl::sleep_for(sleep_ticks);
        }
        Ok(())
    }
}

impl<ImplT: Ecp5Impl> From<ImplT> for Ecp5<ImplT> {
    fn from(i: ImplT) -> Self {
        Ecp5(i)
    }
}

pub const DEVICE_RESET_DURATION: u64 = 25;
pub const APPLICATION_RESET_DURATION: u64 = 25;
pub const BUSY_DURATION: u64 = 10;
pub const DONE_DURATION: u64 = 10;

/// Implement the FPGA trait for ECP5, allowing the device to be exposed through
/// the FPGA server.
impl<ImplT: Ecp5Impl> Fpga for Ecp5<ImplT>
where
    FpgaError: From<<ImplT as Ecp5Impl>::Error>,
{
    fn device_enabled(&self) -> Result<bool, FpgaError> {
        Ok(self.0.program_n()?)
    }

    fn set_device_enable(&mut self, enabled: bool) -> Result<(), FpgaError> {
        self.0.set_program_n(enabled)?;
        ringbuf_entry!(if enabled {
            Trace::Enabled
        } else {
            Trace::Disabled
        });
        Ok(())
    }

    fn reset_device(&mut self, ticks: u64) -> Result<(), FpgaError> {
        self.set_device_enable(false)?;
        hl::sleep_for(ticks);
        self.set_device_enable(true)?;
        Ok(())
    }

    fn device_state(&self) -> Result<DeviceState, FpgaError> {
        if !self.0.program_n()? {
            Ok(DeviceState::Disabled)
        } else {
            if self.0.done()? {
                Ok(DeviceState::RunningApplication)
            } else {
                if self.0.init_n()? {
                    Ok(DeviceState::AwaitingBitstream)
                } else {
                    Ok(DeviceState::Error)
                }
            }
        }
    }

    fn device_id(&self) -> Result<u32, FpgaError> {
        Ok(self.read32(Command::ReadId)?)
    }

    fn start_bitstream_load(&mut self) -> Result<(), FpgaError> {
        // Put device in configuration mode if required.
        if !self.status()?.write_enabled() {
            self.enable_configuration_mode()?;
        }

        if !self.status()?.write_enabled() {
            return Err(FpgaError::InvalidState);
        }

        // Assert the design reset.
        self.set_application_enable(false)?;

        self.0.lock()?;
        // Use the Impl to write the command and leave the device locked for the
        // byte stream to follow.
        self.0.write_command(Command::BitstreamBurst)?;
        ringbuf_entry!(Trace::Command(Command::BitstreamBurst));

        Ok(())
    }

    fn continue_bitstream_load(&mut self, buf: &[u8]) -> Result<(), FpgaError> {
        self.0.write(buf)?;
        ringbuf_entry!(Trace::WriteBitstreamChunk);
        Ok(())
    }

    fn finish_bitstream_load(
        &mut self,
        application_reset_ticks: u64,
    ) -> Result<(), FpgaError> {
        self.0.release()?;
        self.await_not_busy(BUSY_DURATION)?;

        // Perform climb-out checklist; determine if the bitstream was accepted
        // and the device is ready for wake up.
        let status = self.status()?;

        if status.encrypt_preamble_detected() {
            ringbuf_entry!(Trace::EncryptedBitstreamDetected);
        }
        if status.standard_preamble_detected() {
            ringbuf_entry!(Trace::StandardBitstreamDetected);
        }

        let error = status.bitstream_error();
        if error != BitstreamError::None {
            // Log and bail. This leaves the device in configuration mode (and
            // the SPI port enabled), allowing the caller to issue a Refresh
            // command and try again if so desired.
            ringbuf_entry!(Trace::BitstreamError(error));
            return Err(FpgaError::BitstreamError(error as u8));
        }

        ringbuf_entry!(Trace::BitstreamError(BitstreamError::None));

        // Return to user mode, initiating the control sequence which will start
        // the fabric. Completion of this transition is externally observable
        // with the DONE pin going high.
        //
        // Unless the port is set to remain enabled through the FAE bits it will
        // be disabled at this point, i.e. performing a read of the ID or Status
        // registers will result in a PortDisabled error.
        self.disable_configuration_mode()?;

        self.await_done(DONE_DURATION)?;

        hl::sleep_for(application_reset_ticks);
        self.set_application_enable(true)?;

        Ok(())
    }

    fn application_enabled(&self) -> Result<bool, FpgaError> {
        Ok(!self.0.application_reset_n()?)
    }

    fn set_application_enable(
        &mut self,
        enabled: bool,
    ) -> Result<(), FpgaError> {
        self.0.set_application_reset_n(enabled)?;
        ringbuf_entry!(if enabled {
            Trace::ApplicationResetDeasserted
        } else {
            Trace::ApplicationResetAsserted
        });
        Ok(())
    }

    fn reset_application(&mut self, ticks: u64) -> Result<(), FpgaError> {
        self.set_application_enable(false)?;
        hl::sleep_for(ticks);
        self.set_application_enable(true)?;
        Ok(())
    }
}
