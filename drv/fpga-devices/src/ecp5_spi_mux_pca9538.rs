// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This module implements an ECP5 driver which exposes two physical devices,
//! which share a single SPI bus using a mux and are controlled through a shared
//! PCA9538 GPIO expander.

use crate::ecp5::{Command, Ecp5, Ecp5Driver};
use crate::FpgaUserDesign;
use drv_fpga_api::FpgaError;
use drv_i2c_api::ResponseCode;
use drv_i2c_devices::pca9538;
use drv_spi_api::{self as spi_api, SpiDevice, SpiError, SpiServer};
use drv_stm32xx_sys_api::{self as sys_api, Sys};

/// Impl Error type, with conversion from `SpiError` and `ResponseCode`.
#[derive(Copy, Clone, Debug)]
pub enum Error {
    SpiError(SpiError),
    I2cError(ResponseCode),
}

impl From<SpiError> for Error {
    fn from(e: SpiError) -> Self {
        Self::SpiError(e)
    }
}

impl From<ResponseCode> for Error {
    fn from(e: ResponseCode) -> Self {
        Self::I2cError(e)
    }
}

impl From<Error> for u8 {
    fn from(e: Error) -> Self {
        match e {
            Error::SpiError(e) => match e {
                SpiError::BadTransferSize => 3,
                SpiError::TaskRestarted => 4,
            },
            Error::I2cError(e) => 8 + (e as u8),
        }
    }
}

impl From<Error> for FpgaError {
    fn from(e: Error) -> Self {
        FpgaError::ImplError(u8::from(e))
    }
}

pub struct DriverConfig<S: SpiServer> {
    pub sys: Sys,
    pub configuration_port: SpiDevice<S>,
    pub user_design: SpiDevice<S>,
    pub spi_mux_select: sys_api::PinSet,
    pub gpio: pca9538::Pca9538,
    pub user_design_reset_duration: u64,
}

pub struct DevicePins {
    pub done: pca9538::PinSet,
    pub init_n: pca9538::PinSet,
    pub program_n: pca9538::PinSet,
    pub user_design_reset_n: pca9538::PinSet,
}

pub struct DeviceInstance<'a, S: SpiServer> {
    pub driver: &'a Driver<S>,
    pub device_id: usize,
    pub pins: DevicePins,
}

pub struct Driver<S: SpiServer> {
    config: DriverConfig<S>,
    device_selected: core::cell::Cell<Option<usize>>,
}

impl<S: SpiServer> Driver<S> {
    pub fn new(config: DriverConfig<S>) -> Self {
        Self {
            config,
            device_selected: core::cell::Cell::new(None),
        }
    }

    pub fn init(&self) -> Result<(), Error> {
        use sys_api::*;

        self.config.sys.gpio_set(self.config.spi_mux_select);
        self.config.sys.gpio_configure_output(
            self.config.spi_mux_select,
            OutputType::PushPull,
            Speed::Low,
            Pull::None,
        );
        self.device_selected.set(Some(0));
        Ok(())
    }

    fn init_device(
        &self,
        device_id: usize,
        pins: DevicePins,
    ) -> Result<DeviceInstance<'_, S>, Error> {
        let output_pins =
            pins.init_n | pins.program_n | pins.user_design_reset_n;

        self.config.gpio.set_mode(
            output_pins,
            pca9538::Mode::Output,
            pca9538::Polarity::Normal,
        )?;

        self.config.gpio.set_mode(
            pins.done,
            pca9538::Mode::Input,
            pca9538::Polarity::Normal,
        )?;

        Ok(DeviceInstance {
            driver: self,
            device_id,
            pins,
        })
    }

    pub fn init_devices(
        &self,
        device0_pins: DevicePins,
        device1_pins: DevicePins,
    ) -> Result<[Ecp5<DeviceInstance<'_, S>>; 2], Error> {
        Ok([
            Ecp5::new(self.init_device(0, device0_pins)?),
            Ecp5::new(self.init_device(1, device1_pins)?),
        ])
    }

    pub fn select_device(&self, device_id: usize) {
        let set_gpio = || {
            self.config
                .sys
                .gpio_set_to(self.config.spi_mux_select, device_id == 0);
            self.device_selected.set(Some(device_id));
            userlib::hl::sleep_for(1);
        };

        match self.device_selected.get() {
            None => set_gpio(),
            Some(selected_id) if selected_id != device_id => set_gpio(),
            _ => (),
        }
    }
}

impl<'a, S: SpiServer> Ecp5Driver for DeviceInstance<'a, S> {
    type Error = Error;

    fn program_n(&self) -> Result<bool, Self::Error> {
        Ok(self.driver.config.gpio.read(self.pins.program_n)? != 0)
    }

    fn set_program_n(&self, asserted: bool) -> Result<(), Self::Error> {
        self.driver
            .config
            .gpio
            .set_to(self.pins.program_n, asserted)
            .map_err(Self::Error::from)
    }

    fn init_n(&self) -> Result<bool, Self::Error> {
        Ok(self.driver.config.gpio.read(self.pins.init_n)? != 0)
    }

    fn set_init_n(&self, asserted: bool) -> Result<(), Self::Error> {
        self.driver
            .config
            .gpio
            .set_to(self.pins.init_n, asserted)
            .map_err(Self::Error::from)
    }

    fn done(&self) -> Result<bool, Self::Error> {
        Ok(self.driver.config.gpio.read(self.pins.done)? != 0)
    }

    fn set_done(&self, asserted: bool) -> Result<(), Self::Error> {
        self.driver
            .config
            .gpio
            .set_to(self.pins.done, asserted)
            .map_err(Self::Error::from)
    }

    fn user_design_reset_n(&self) -> Result<bool, Self::Error> {
        Ok(self
            .driver
            .config
            .gpio
            .read(self.pins.user_design_reset_n)?
            != 0)
    }

    fn set_user_design_reset_n(&self, val: bool) -> Result<(), Self::Error> {
        Ok(self
            .driver
            .config
            .gpio
            .set_to(self.pins.user_design_reset_n, val)?)
    }

    fn user_design_reset_duration(&self) -> u64 {
        self.driver.config.user_design_reset_duration
    }

    fn configuration_read(&self, data: &mut [u8]) -> Result<(), Self::Error> {
        self.driver.select_device(self.device_id);
        self.driver
            .config
            .configuration_port
            .read(data)
            .map_err(Self::Error::from)
    }

    fn configuration_write(&self, data: &[u8]) -> Result<(), Self::Error> {
        self.driver.select_device(self.device_id);
        self.driver
            .config
            .configuration_port
            .write(data)
            .map_err(Self::Error::from)
    }

    fn configuration_write_command(
        &self,
        c: Command,
    ) -> Result<(), Self::Error> {
        let buffer: [u8; 4] = [c as u8, 0, 0, 0];
        self.driver.select_device(self.device_id);
        self.driver
            .config
            .configuration_port
            .write(&buffer)
            .map_err(Self::Error::from)
    }

    fn configuration_lock(&self) -> Result<(), Self::Error> {
        self.driver.select_device(self.device_id);
        self.driver
            .config
            .configuration_port
            .lock(spi_api::CsState::Asserted)
            .map_err(|_| Self::Error::from(SpiError::TaskRestarted))
    }

    fn configuration_release(&self) -> Result<(), Self::Error> {
        self.driver
            .config
            .configuration_port
            .release()
            .map_err(|_| Self::Error::from(SpiError::TaskRestarted))
    }
}

impl<'a, S: SpiServer> FpgaUserDesign for DeviceInstance<'a, S> {
    fn user_design_enabled(&self) -> Result<bool, FpgaError> {
        Ok(self.user_design_reset_n()?)
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
        self.driver.select_device(self.device_id);
        self.driver
            .config
            .user_design
            .read(data)
            .map_err(Into::into)
    }

    fn user_design_write(&self, data: &[u8]) -> Result<(), FpgaError> {
        self.driver.select_device(self.device_id);
        self.driver
            .config
            .user_design
            .write(data)
            .map_err(Into::into)
    }

    fn user_design_lock(&self) -> Result<(), FpgaError> {
        self.driver.select_device(self.device_id);
        self.driver
            .config
            .user_design
            .lock(spi_api::CsState::Asserted)
            .map_err(|_| SpiError::TaskRestarted.into())
    }

    fn user_design_release(&self) -> Result<(), FpgaError> {
        self.driver
            .config
            .user_design
            .release()
            .map_err(|_| SpiError::TaskRestarted.into())
    }
}
