// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::*;

#[cfg_attr(
    any(
        target_board = "sidecar-b",
        target_board = "sidecar-c",
        target_board = "sidecar-d"
    ),
    path = "clock_generator_payload_bcd.rs"
)]
mod payload;

pub(crate) struct ClockGenerator {
    pub device: I2cDevice,
    pub config_loaded: bool,
}

impl ClockGenerator {
    pub fn new(i2c_task: userlib::TaskId) -> Self {
        Self {
            device: i2c_config::devices::idt8a34001(i2c_task)[0],
            config_loaded: false,
        }
    }

    pub fn load_config(&mut self) -> Result<(), SeqError> {
        ringbuf_entry!(Trace::LoadingClockConfiguration);

        let mut packet = 0;

        payload::idt8a3xxxx_payload(|buf| match self.device.write(buf) {
            Err(err) => {
                ringbuf_entry!(Trace::ClockConfigurationError(packet, err));
                Err(SeqError::ClockConfigurationFailed)
            }

            Ok(_) => {
                packet += 1;
                Ok(())
            }
        })?;

        self.config_loaded = true;
        Ok(())
    }
}
