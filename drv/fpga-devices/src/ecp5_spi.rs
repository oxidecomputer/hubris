// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::ecp5::{Command, Ecp5Driver};
use crate::FpgaUserDesign;
use drv_fpga_api::FpgaError;
use drv_spi_api::{self as spi_api, SpiDevice, SpiError};
use drv_stm32xx_sys_api::{self as sys_api, GpioError, Sys};

/// `Ecp5UsingSpi` is the simplest implementation of the Ecp5Impl interface using
/// the SPI and Sys APIs. It assumes the PROGRAM_N, INIT_N and DONE signals are
/// directly connected to GPIO pins.

pub struct Ecp5UsingSpi {
    pub sys: Sys,
    pub configuration_port: SpiDevice,
    pub user_design: SpiDevice,
    pub done: sys_api::PinSet,
    pub init_n: sys_api::PinSet,
    pub program_n: sys_api::PinSet,
    pub user_design_reset_n: sys_api::PinSet,
    pub user_design_reset_duration: u64,
}

/// Impl Error type, with conversion from GpioError and SpiError.
#[derive(Copy, Clone, Debug)]
pub enum Ecp5UsingSpiError {
    GpioError(GpioError),
    SpiError(SpiError),
}

impl From<GpioError> for Ecp5UsingSpiError {
    fn from(e: GpioError) -> Self {
        Self::GpioError(e)
    }
}

impl From<SpiError> for Ecp5UsingSpiError {
    fn from(e: SpiError) -> Self {
        Self::SpiError(e)
    }
}

impl From<Ecp5UsingSpiError> for u8 {
    fn from(e: Ecp5UsingSpiError) -> Self {
        match e {
            Ecp5UsingSpiError::GpioError(e) => match e {
                GpioError::BadArg => 2,
            },
            Ecp5UsingSpiError::SpiError(e) => match e {
                SpiError::BadTransferSize => 3,
                SpiError::ServerRestarted => 4,
                SpiError::NothingToRelease => 5,
                SpiError::BadDevice => 6,
            },
        }
    }
}

impl From<Ecp5UsingSpiError> for FpgaError {
    fn from(e: Ecp5UsingSpiError) -> Self {
        FpgaError::ImplError(u8::from(e))
    }
}

impl Ecp5Driver for Ecp5UsingSpi {
    type Error = Ecp5UsingSpiError;

    fn program_n(&self) -> Result<bool, Self::Error> {
        Ok(self.sys.gpio_read(self.program_n)? != 0)
    }

    fn set_program_n(&self, asserted: bool) -> Result<(), Self::Error> {
        Ok(self.sys.gpio_set_to(self.program_n, asserted)?)
    }

    fn init_n(&self) -> Result<bool, Self::Error> {
        Ok(self.sys.gpio_read(self.init_n)? != 0)
    }

    fn set_init_n(&self, asserted: bool) -> Result<(), Self::Error> {
        Ok(self.sys.gpio_set_to(self.init_n, !asserted)?)
    }

    fn done(&self) -> Result<bool, Self::Error> {
        Ok(self.sys.gpio_read(self.done)? != 0)
    }

    fn set_done(&self, asserted: bool) -> Result<(), Self::Error> {
        Ok(self.sys.gpio_set_to(self.done, asserted)?)
    }

    fn user_design_reset_n(&self) -> Result<bool, Self::Error> {
        Ok(self.sys.gpio_read(self.user_design_reset_n)? != 0)
    }

    fn set_user_design_reset_n(
        &self,
        asserted: bool,
    ) -> Result<(), Self::Error> {
        Ok(self.sys.gpio_set_to(self.user_design_reset_n, asserted)?)
    }

    fn user_design_reset_duration(&self) -> u64 {
        self.user_design_reset_duration
    }

    fn configuration_read(&self, data: &mut [u8]) -> Result<(), Self::Error> {
        Ok(self.configuration_port.read(data)?)
    }

    fn configuration_write(&self, data: &[u8]) -> Result<(), Self::Error> {
        Ok(self.configuration_port.write(data)?)
    }

    fn configuration_write_command(
        &self,
        c: Command,
    ) -> Result<(), Self::Error> {
        let buffer: [u8; 4] = [c as u8, 0, 0, 0];
        Ok(self.configuration_port.write(&buffer)?)
    }

    fn configuration_lock(&self) -> Result<(), Self::Error> {
        Ok(self.configuration_port.lock(spi_api::CsState::Asserted)?)
    }

    fn configuration_release(&self) -> Result<(), Self::Error> {
        Ok(self.configuration_port.release()?)
    }
}

impl FpgaUserDesign for Ecp5UsingSpi {
    fn user_design_enabled(&self) -> Result<bool, FpgaError> {
        Ok(!self.user_design_reset_n()?)
    }

    fn set_user_design_enabled(&self, enabled: bool) -> Result<(), FpgaError> {
        use crate::ecp5::{ecp5_trace, Trace};

        self.set_user_design_reset_n(enabled)?;

        ecp5_trace(if enabled {
            Trace::UserDesignResetDeasserted
        } else {
            Trace::UserDesignResetAsserted
        });

        Ok(())
    }

    fn reset_user_design(&self) -> Result<(), FpgaError> {
        self.set_user_design_enabled(false)?;
        userlib::hl::sleep_for(self.user_design_reset_duration());
        self.set_user_design_enabled(true)?;
        Ok(())
    }

    fn user_design_read(&self, data: &mut [u8]) -> Result<(), FpgaError> {
        Ok(self.user_design.read(data)?)
    }

    fn user_design_write(&self, data: &[u8]) -> Result<(), FpgaError> {
        Ok(self.user_design.write(data)?)
    }

    fn user_design_lock(&self) -> Result<(), FpgaError> {
        Ok(self.user_design.lock(spi_api::CsState::Asserted)?)
    }

    fn user_design_release(&self) -> Result<(), FpgaError> {
        Ok(self.user_design.release()?)
    }
}

impl Ecp5UsingSpi {
    pub fn configure_gpio(&self) {
        use sys_api::*;

        self.sys.gpio_set(self.done).unwrap();
        self.sys
            .gpio_configure_output(
                self.done,
                OutputType::OpenDrain,
                Speed::Low,
                Pull::Up,
            )
            .unwrap();

        self.sys.gpio_set(self.init_n).unwrap();
        self.sys
            .gpio_configure_output(
                self.init_n,
                OutputType::OpenDrain,
                Speed::Low,
                Pull::Up,
            )
            .unwrap();

        self.sys.gpio_set(self.program_n).unwrap();
        self.sys
            .gpio_configure_output(
                self.program_n,
                OutputType::OpenDrain,
                Speed::Low,
                Pull::None,
            )
            .unwrap();

        self.sys.gpio_set(self.user_design_reset_n).unwrap();
        self.sys
            .gpio_configure_output(
                self.user_design_reset_n,
                OutputType::OpenDrain,
                Speed::Low,
                Pull::None,
            )
            .unwrap();
    }
}
