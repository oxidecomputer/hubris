// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common types and helpers for Emc2305 Fan Controller

use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::emc2305::Emc2305;
use ringbuf::ringbuf_entry_root;
use task_sensor_api::SensorId;
use task_thermal_api::{FanProperties, SensorReadError, ThermalError};

use crate::{
    Trace,
    control::{Fan, FanPresentState, FanState},
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
        retry_init(|| this.initialize().map(drop));
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

pub(crate) fn update_fan(
    now: u64,
    fctrl: &mut Emc2305,
    fan: &mut Fan<drv_i2c_devices::emc2305::Fan>,
    model: &FanProperties,
) {
    // If this fan is not present, then do not attempt to poll it. Presence is
    // only restored via presence polling.
    if !fan.is_present() {
        return;
    }

    // Try to get the RPM reading for this fan
    let res = fctrl.fan_rpm(fan.bsp_data);
    match res {
        Ok(rpm) => {
            // The poll went well! Use the model to determine if this reading
            // is nominal or not, and report that as the state.
            let state = if rpm < model.underspeed_rpm {
                FanPresentState::TooSlow(rpm)
            } else if rpm > model.underspeed_rpm {
                FanPresentState::TooFast(rpm)
            } else {
                FanPresentState::Nominal(rpm)
            };
            fan.update_state(FanState::Present(state), now);
        }
        Err(_e) => {
            // No good, mark as unresponsive
            fan.update_state(
                FanState::Present(FanPresentState::Unresponsive),
                now,
            );
        }
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
        out[idx].cur_state = FanState::Present(FanPresentState::Unresponsive);
        out[idx].presence_acked = true;
        idx += 1;
    }

    out
}
