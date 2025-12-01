// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Fault reporting

use task_jefe_api::FaultReport;
use userlib::kipc;

pub const MAX_BUFFERED: usize = 32;

pub(crate) struct FaultReports {
    buf: &'static mut heapless::Deque<FaultReport, MAX_BUFFERED>,
    lost: Option<u32>,
}

impl FaultReports {
    pub fn claim_static_resources() -> Self {
        use static_cell::ClaimOnceCell;

        static BUF: ClaimOnceCell<heapless::Deque<FaultReport, MAX_BUFFERED>> =
            ClaimOnceCell::new();
        let buf = BUF.claim().unwrap();
        Self { buf, lost: None }
    }

    pub fn record_fault(&mut self, task: usize) {
        let status = kipc::read_task_status(task);
        todo!();
    }
}
