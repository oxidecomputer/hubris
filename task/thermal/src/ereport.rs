// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use fixedstr::FixedStr;
use ringbuf::ringbuf_entry_root;
use task_packrat_api::Packrat;
use task_sensor_api::config::MAX_COMPONENT_ID_LEN;
use userlib::task_slot;

task_slot!(PACKRAT, packrat);

pub(crate) const EREPORT_BUF_SIZE: usize =
    microcbor::max_cbor_len_for![Ereport,];

pub(crate) struct Ereporter {
    buf: &'static mut [u8; EREPORT_BUF_SIZE],
    pending: &'static mut Option<Ereport>,
    packrat: Packrat,
}

impl Ereporter {
    pub(crate) fn claim_static_resources() -> Self {
        use static_cell::ClaimOnceCell;

        static BUF: ClaimOnceCell<[u8; EREPORT_BUF_SIZE]> =
            ClaimOnceCell::new([0u8; EREPORT_BUF_SIZE]);
        static PENDING: ClaimOnceCell<Option<Ereport>> =
            ClaimOnceCell::new(None);

        Self {
            buf: BUF.claim(),
            pending: PENDING.claim(),
            packrat: Packrat::from(PACKRAT.get_task_id()),
        }
    }

    pub(crate) fn pending_mut(&mut self) -> &mut Option<Ereport> {
        self.pending
    }

    pub(crate) fn flush_pending(&mut self) {
        let Some(ereport) = self.pending.as_ref() else {
            return;
        };
        let eresult =
            self.packrat.deliver_microcbor_ereport(&ereport, self.buf);
        match eresult {
            Ok(len) => {
                ringbuf_entry_root!(Trace::EreportSent { len });
                self.pending = None;
            }
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
        sensor_id: u32,
        temp_c: f32,
        time: u64,
    },
    /// A component exceeded its power-down threshold.
    #[cbor(rename = "hw.temp.pwrdown")]
    ComponentPowerDown {
        #[cbor(rename = "v")]
        version: u8,
        refdes: FixedStr<'static, { MAX_COMPONENT_ID_LEN }>,
        sensor_id: u32,
        temp_c: f32,
        overheat_ms: Option<OverheatDurations>,
        time: u64,
    },
    /// The system is shutting down due to exceeding the critical threshold
    /// timeout.
    #[cbor(rename = "hw.temp.crit.timeout")]
    TimeoutShutdown {
        #[cbor(rename = "v")]
        version: u8,
        overheat_ms: OverheatDurations,
        time: u64,
    },
    /// All temperatures have returned to nominal.
    #[cbor(rename = "hw.temp.ok")]
    Nominal {
        #[cbor(rename = "v")]
        version: u8,
        overheat_ms: OverheatDurations,
        time: u64,
    },
    #[cbor(rename = "hw.temp.readerr")]
    SensorError {
        #[cbor(rename = "v")]
        version: u8,
        refdes: FixedStr<'static, { MAX_COMPONENT_ID_LEN }>,
        sensor_id: u32,
        time: u64,
    },
}

#[derive(microcbor::Encode)]
pub struct OverheatDurations {
    pub crit: u64,
    pub total: u64,
}
