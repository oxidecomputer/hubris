// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Ereport type definitions used by multiple BSPs.

// Which ereport types are used depends on the board.
#![allow(dead_code)]

use crate::sensor_api::SensorId;

#[derive(serde::Serialize)]
pub(crate) struct VoutSag {
    k: &'static str,
    rail: &'static str,
    time: u32,
    sensor_id: u32,
    vout_min: f32,
    threshold: f32,
}

impl VoutSag {
    pub(crate) fn new(
        rail: &'static str,
        time: u32,
        sensor_id: SensorId,
        vout_min: f32,
        threshold: f32,
    ) -> Self {
        Self {
            k: "pwr.vout_under_threshold",
            rail,
            time,
            sensor_id: sensor_id.into(),
            vout_min,
            threshold,
        }
    }
}
