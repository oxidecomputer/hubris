// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common types and helpers for Max31790 Fan Controller

use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::max31790::Fan as MaxFan;
use drv_i2c_devices::max31790::Max31790;
use ringbuf::ringbuf_entry_root;
use task_sensor_api::SensorId;
use task_thermal_api::{
    SANYO_DENKI_FAN_PROPERTIES, SensorReadError, ThermalError,
};

use crate::{
    Trace,
    control::{FanPresentState, FanState},
};

/// Tracks whether a MAX31790 fan controller has been initialized, and
/// initializes it on demand when accessed, if necessary.
///
/// Because initializing the fan controller can fail due to a transient bus
/// error, we don't panic if an initial attempt to initialize it as soon as the
/// `thermal` task starts fails. Because the fan controller's I2C watchdog will
/// simply run the fans at 100% if we aren't able to talk to it right away, the
/// `thermal` task should keep running, publishing sensor measurements, and
/// periodically trying to reach the fan controller until we're able to
/// initialize it successfully. Thus, we wrap it in this struct to track whether
/// it's been successfully initialized yet.
pub(crate) struct Max31790State {
    max31790: Max31790,
    initialized: bool,
}

impl Max31790State {
    #[allow(dead_code)]
    pub(crate) fn new(dev: &I2cDevice) -> Self {
        let mut this = Self {
            max31790: Max31790::new(dev),
            initialized: false,
        };
        retry_init(|| this.initialize().map(drop));
        this
    }

    /// Access the fan controller, attempting to initialize it if it has not yet
    /// been initialized.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn try_initialize(
        &mut self,
    ) -> Result<&mut Max31790, ControllerInitError> {
        if self.initialized {
            return Ok(&mut self.max31790);
        }

        self.initialize()
    }

    // Slow path that actually performs initialization. This is "outlined" so
    // that we can avoid pushing a stack frame in the case where we just need to
    // check a bool and return a pointer.
    #[inline(never)]
    fn initialize(&mut self) -> Result<&mut Max31790, ControllerInitError> {
        self.max31790.initialize().map_err(|e| {
            ringbuf_entry_root!(Trace::FanControllerInitError(e));
            ControllerInitError(e)
        })?;

        self.initialized = true;
        ringbuf_entry_root!(Trace::FanControllerInitialized);
        Ok(&mut self.max31790)
    }
}

/// Helper function to retry initialization several times, logging errors
pub(crate) fn retry_init<F: FnMut() -> Result<(), ControllerInitError>>(
    mut init: F,
) {
    // When we first start up, try to initialize the fan controller a few
    // times, in case there's a transient I2C error.
    for remaining in (0..3).rev() {
        if init().is_ok() {
            break;
        }
        ringbuf_entry_root!(Trace::FanControllerInitRetry { remaining });
    }
}

pub(crate) struct ControllerInitError(pub(crate) ResponseCode);

impl From<ControllerInitError> for ThermalError {
    fn from(_: ControllerInitError) -> Self {
        ThermalError::FanControllerUninitialized
    }
}

impl From<ControllerInitError> for SensorReadError {
    fn from(ControllerInitError(code): ControllerInitError) -> Self {
        SensorReadError::I2cError(code)
    }
}

#[allow(dead_code)]
pub(crate) const fn make_consecutive_nonremovable_fans<const N: usize>(
    sensors: &'static [SensorId; N],
) -> [crate::control::Fan<MaxFan>; N] {
    const ONE: crate::control::Fan<MaxFan> = crate::control::Fan::new(
        SensorId::new(0),
        SANYO_DENKI_FAN_PROPERTIES,
        MaxFan::new_const(0),
    );

    let mut out = [ONE; N];
    let mut idx = 0;
    while idx < N {
        out[idx].rpm_sensor_id = sensors[idx];
        out[idx].bsp_data = MaxFan::new_const(idx as u8);
        out[idx].cur_state = FanState::Present(FanPresentState::Unpolled);
        out[idx].presence_acked = true;
        idx += 1;
    }

    out
}
