// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common types and helpers for Emc2305 Fan Controller

use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::emc2305::Emc2305;
use ringbuf::ringbuf_entry_root;
use task_sensor_api::SensorId;
use task_thermal_api::{SensorReadError, ThermalError};

use crate::{
    Trace,
    control::{Fan, FanStatus},
};

/// Tracks whether a Emc2305 fan controller has been initialized, and
/// initializes it on demand when accessed, if necessary.
pub(crate) struct Emc2305State {
    emc2305: Emc2305,
    fan_count: u8,
    initialized: bool,
}

impl Emc2305State {
    #[allow(dead_code)]
    pub(crate) fn new(dev: &I2cDevice, fan_count: u8) -> Self {
        let mut this = Self {
            emc2305: Emc2305::new(dev),
            fan_count,
            initialized: false,
        };
        retry_init(|| this.initialize().map(|_| ()));
        this
    }

    /// Access the fan controller, attempting to initialize it if it has not yet
    /// been initialized.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn try_initialize(
        &mut self,
    ) -> Result<&mut Emc2305, ControllerInitError> {
        if self.initialized {
            return Ok(&mut self.emc2305);
        }

        self.initialize()
    }

    // Slow path that actually performs initialization. This is "outlined" so
    // that we can avoid pushing a stack frame in the case where we just need to
    // check a bool and return a pointer.
    #[inline(never)]
    fn initialize(&mut self) -> Result<&mut Emc2305, ControllerInitError> {
        self.emc2305.initialize(self.fan_count).map_err(|e| {
            ringbuf_entry_root!(Trace::FanControllerInitError(e));
            ControllerInitError(e)
        })?;

        self.initialized = true;
        ringbuf_entry_root!(Trace::FanControllerInitialized);
        Ok(&mut self.emc2305)
    }

    pub(crate) fn read_fan_rpms(
        &mut self,
        fans: &mut [Fan<drv_i2c_devices::emc2305::Fan>],
    ) -> impl Iterator<Item = FanStatus> {
        // Try to initialize the fan controller once at the start of the loop
        let mut fctrl = self.try_initialize().map_err(SensorReadError::from);

        // TODO: Maybe there's a way to make this a method on Fan that we can
        // call, kind of like InputStatus?
        fans.iter_mut().map(move |f| {
            let sensor_id = f.rpm_sensor_id;
            if !f.is_present {
                return FanStatus::NotPresent { sensor_id };
            }

            // If initialization failed, then we short circuit to return that
            // original error, copied for each fan we're going to report.
            let fctrl = fctrl.as_mut().map_err(|e| *e);

            // If it was a success, attempt to read the RPMs, and either report
            // that success or that error for each fan rpm.
            let res = fctrl.and_then(|fc| {
                fc.fan_rpm(f.bsp_data).map_err(SensorReadError::I2cError)
            });
            match res {
                Ok(rpm) => FanStatus::PresentSuccess { rpm, sensor_id },
                Err(error) => FanStatus::PresentError { error, sensor_id },
            }
        })
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
) -> [crate::control::Fan<drv_i2c_devices::emc2305::Fan>; N] {
    const ONE: crate::control::Fan<drv_i2c_devices::emc2305::Fan> =
        crate::control::Fan::new(
            SensorId::new(0),
            drv_i2c_devices::emc2305::Fan::new_const(0),
        );

    let mut out = [ONE; N];
    let mut idx = 0;
    while idx < N {
        out[idx] = crate::control::Fan::new(
            sensors[idx],
            drv_i2c_devices::emc2305::Fan::new_const(idx as u8),
        );
        out[idx].is_present = true;
        idx += 1;
    }

    out
}
