// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common types and helpers for Emc2305 Fan Controller

use drv_i2c_api::I2cDevice;
use drv_i2c_devices::emc2305::Emc2305;
use ringbuf::ringbuf_entry;

use crate::{
    Trace,
    control::{ControllerInitError, retry_init},
};

/// Tracks whether a Emc2305 fan controller has been initialized, and
/// initializes it on demand when accessed, if necessary.
///
/// This is copy-pasted from [`Max31790`]
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
            ringbuf_entry!(Trace::FanControllerInitError(e));
            ControllerInitError(e)
        })?;

        self.initialized = true;
        ringbuf_entry!(Trace::FanControllerInitialized);
        Ok(&mut self.emc2305)
    }
}
