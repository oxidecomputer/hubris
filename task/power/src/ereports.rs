// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Ereport type definitions used by multiple BSPs.

// Which ereport types are used depends on the board.
#![allow(dead_code)]

use crate::sensor_api::SensorId;

#[derive(serde::Serialize)]
pub(crate) struct Crossbounce {
    k: &'static str,
    pub(crate) rail: &'static str,
    pub(crate) iout: Option<Peaks>,
    pub(crate) vout: Option<Peaks>,
    pub(crate) time: u32,
    pub(crate) sensor_id: u32,
}

#[derive(serde::Serialize)]
pub(crate) struct Peaks {
    pub(crate) min: f32,
    pub(crate) max: f32,
}

impl Crossbounce {
    pub(crate) fn new(
        rail: &'static str,
        time: u32,
        sensor_id: SensorId,
    ) -> Self {
        Self {
            k: "pwr.xbounce",
            rail,
            iout: None,
            vout: None,
            time,
            sensor_id: sensor_id.into(),
        }
    }
}
