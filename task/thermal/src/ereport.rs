// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::i2c_config::MAX_COMPONENT_ID_LEN;

pub(crate) const EREPORT_BUF_SIZE: usize =
    microcbor::max_cbor_len_for![Ereport,];

#[derive(microcbor::Encode)]
#[cbor(variant_id = "k")]
pub enum Ereport {
    /// A component exceeded its critical threshold.
    #[cbor(rename = "hw.temp.crit")]
    ComponentCritical {
        #[cbor(rename = "v")]
        version: u8,
        refdes: FixedStr<'static, MAX_COMPONENT_ID_LEN>,
        sensor_id: u8,
        temp_c: f32,
    },
    /// A component exceeded its power-down threshold.
    #[cbor(rename = "hw.temp.a2.thresh")]
    ComponentShutdown {
        #[cbor(rename = "v")]
        version: u8,
        refdes: FixedStr<'static, MAX_COMPONENT_ID_LEN>,
        sensor_id: u8,
        temp_c: f32,
        overheat_ms: Option<OverheatDurations>,
    },
    /// The system is shutting down due to exceeding the critical threshold
    /// timeout.
    #[cbor(rename = "hw.temp.a2.timeout")]
    TimeoutShutdown {
        #[cbor(rename = "v")]
        version: u8,
        overheat_ms: OverheatDurations,
    },
    /// All temperatures have returned to nominal.
    #[cbor(rename = "hw.temp.ok")]
    Nominal {
        #[cbor(rename = "v")]
        version: u8,
        overheat_ms: OverheatDurations,
    },
    #[cbor(rename = "hw.temp.readerr")]
    SensorError {
        #[cbor(rename = "v")]
        version: u8,
        refdes: FixedStr<'static, MAX_COMPONENT_ID_LEN>,
        sensor_id: u8,
    },
}

#[derive(microcbor::Encode)]
pub struct OverheatDurations {
    pub crit: u64,
    pub total: u64,
}
