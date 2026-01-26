// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::i2c_config::MAX_COMPONENT_ID_LEN;
use crate::Trace;
use fixedstr::FixedStr;
use ringbuf::ringbuf_entry_root;
use task_packrat_api::Packrat;
use userlib::task_slot;

task_slot!(PACKRAT, packrat);

pub(crate) const EREPORT_BUF_SIZE: usize =
    microcbor::max_cbor_len_for![Ereport,];

pub(crate) struct Ereporter {
    buf: &'static mut [u8; EREPORT_BUF_SIZE],
    packrat: Packrat,
}

impl Ereporter {
    pub(crate) fn claim_static_resources() -> Self {
        static BUF: static_cell::ClaimOnceCell<[u8; EREPORT_BUF_SIZE]> =
            static_cell::ClaimOnceCell::new([0u8; EREPORT_BUF_SIZE]);

        Self {
            buf: BUF.claim().unwrap(),
            packrat: Packrat::from(PACKRAT.get_task_id()),
        }
    }

    pub(crate) fn deliver_ereport(&mut self, ereport: &Ereport) {
        let eresult = self.packrat.encode_ereport(&ereport, self.buf);
        match eresult {
            Ok(len) => ringbuf_entry_root!(Trace::EreportSent { len }),
            Err(task_packrat_api::EreportEncodeError::Packrat { len, err }) => {
                ringbuf_entry_root!(Trace::EreportLost { len, err })
            }
            Err(task_packrat_api::EreportEncodeError::Encoder(_)) => {
                ringbuf_entry_root!(Trace::EreportTooBig)
            }
        }
    }
}

#[derive(microcbor::Encode)]
#[cbor(variant_id = "k")]
pub enum Ereport {
    /// A component exceeded its critical threshold.
    #[cbor(rename = "hw.temp.crit")]
    ComponentCritical {
        #[cbor(rename = "v")]
        version: u8,
        refdes: FixedStr<'static, { MAX_COMPONENT_ID_LEN }>,
        sensor_id: u8,
        temp_c: f32,
    },
    /// A component exceeded its power-down threshold.
    #[cbor(rename = "hw.temp.a2.thresh")]
    ComponentShutdown {
        #[cbor(rename = "v")]
        version: u8,
        refdes: FixedStr<'static, { MAX_COMPONENT_ID_LEN }>,
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
        refdes: FixedStr<'static, { MAX_COMPONENT_ID_LEN }>,
        sensor_id: u8,
    },
}

#[derive(microcbor::Encode)]
pub struct OverheatDurations {
    pub crit: u64,
    pub total: u64,
}
