// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Fault reporting

use hubris_num_tasks::Task;
use task_jefe_api::FaultReport;
use userlib::{abi, kipc};

pub const MAX_BUFFERED: usize = 32;

pub(crate) struct FaultReports {
    buf: &'static mut heapless::Deque<FaultReport, MAX_BUFFERED>,
    lost: Option<u32>,
}

impl FaultReports {
    pub(crate) fn claim_static_resources() -> Self {
        use static_cell::ClaimOnceCell;

        static BUF: ClaimOnceCell<heapless::Deque<FaultReport, MAX_BUFFERED>> =
            ClaimOnceCell::new();
        let buf = BUF.claim().unwrap();
        Self { buf, lost: None }
    }

    pub(crate) fn record_fault(&mut self, task: usize) {
        if self.buf.is_full() {
            // Out of space, so just drop it and bail.
            self.lost.get_or_insert(0).saturating_add(1);
            return;
        }

        let Ok(task) = Task::try_from(task) else {
            // Well, that's weird and bad; task indices should always be in
            // range. But let's not panic the supervisor about it, I guess...
            return;
        };

        let state = kipc::read_task_status(task);
        let report = match status {
            abi::TaskState::Healthy(_) => {
                // Well, this is weird: it should be faulted. I guess let's do
                // nothing, instead of panicking the supervisor about it...
                return;
            }
            abi::TaskState::Faulted {
                fault,
                original_state,
            } => {
                todo!()
            }
        };
        todo!();
    }

    pub(crate) fn next_fault(&self) -> Option<&FaultReport> {
        self.buf.front()
    }

    pub(crate) fn flush_fault(&mut self) -> bool {
        self.buf.pop_front();
        !self.buf.is_empty()
    }
}
