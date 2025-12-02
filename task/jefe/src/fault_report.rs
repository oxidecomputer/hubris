// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Fault reporting

use hubris_num_tasks::Task;
use task_jefe_api::FaultReport;
use userlib::{self, kipc};

pub const MAX_BUFFERED: usize = 16;

pub(crate) struct FaultReports {
    buf: &'static mut heapless::Deque<FaultReport, MAX_BUFFERED>,
    lost: Option<u32>,
}

impl FaultReports {
    pub(crate) fn claim_static_resources() -> Self {
        use static_cell::ClaimOnceCell;

        static BUF: ClaimOnceCell<heapless::Deque<FaultReport, MAX_BUFFERED>> =
            ClaimOnceCell::new(heapless::Deque::new());
        let buf = BUF.claim();
        Self { buf, lost: None }
    }

    pub(crate) fn record_fault(&mut self, task: usize) {
        let state = kipc::read_task_status(task);
        let abi::TaskState::Faulted {
            fault,
            original_state,
        } = state
        else {
            // Well, this is weird: it should be faulted. I guess let's do
            // nothing, instead of panicking the supervisor about it...
            return;
        };

        let Ok(task) = Task::try_from(task) else {
            // Well, that's weird and bad; task indices should always be in
            // range. But let's not panic the supervisor about it, I guess...
            return;
        };

        let now = userlib::sys_get_timer().now;
        if let Some(last) = self.buf.back_mut() {
            // TODO(eliza): check panic message once we have those
            if last.task == task && last.fault == fault {
                last.count = last.count.saturating_add(1);
                last.latest_fault_time = now;
                return;
            }
        }
        let report = FaultReport {
            task,
            fault,
            count: 1,
            latest_fault_time: now,
            initial_fault_time: now,
            panic_message: None, // TODO
        };
        if self.buf.push_back(report).is_err() {
            // Out of space, so just drop it and bail.
            let lost = self.lost.get_or_insert(0);
            *lost = lost.saturating_add(1);
        }
    }

    pub(crate) fn next_fault(&self) -> Option<&FaultReport> {
        self.buf.front()
    }

    pub(crate) fn flush_fault(&mut self) -> bool {
        self.buf.pop_front();
        !self.buf.is_empty()
    }
}
