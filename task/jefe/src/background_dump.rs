// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::notifications;
use core::convert::Infallible;
use ringbuf::*;
use userlib::{sys_get_timer, sys_post, sys_refresh_task_id, TaskId};

pub(super) struct DumpQueue {
    /// Queue of tasks to dump in the background.
    ///
    /// Since a task is not restarted until it has finished being dumped, we can
    /// use `NUM_TASKS` as the size for this queue --- it will never contain
    /// more than `NUM_TASKS` IDs waiting to dump.
    queue: heapless::Deque<u32, { hubris_num_tasks::NUM_TASKS }>,
    jeff: TaskId,
}

#[derive(Debug, Copy, Clone, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    NeedsDump {
        task: u32,
        now: u64,
    },
    StartDump {
        task: u32,
        now: u64,
    },
    DumpFinished {
        task: u32,
        now: u64,
    },
    DumpQueueFull,
    UnexpectedDumpReq,
}

counted_ringbuf!(Trace, 8, Trace::None);

impl DumpQueue {
    pub(super) fn new() -> Self {
        Self {
            queue: heapless::Deque::new(),
            jeff: TaskId::for_index_and_gen(
                super::generated::LITTLE_HELPER as usize,
                userlib::Generation::ZERO,
            ),
        }
    }
    pub(super) fn fault(&mut self, task: u32) {
        if self.queue.push_back(task).is_ok() {
            let now = sys_get_timer().now;
            ringbuf_entry!(Trace::NeedsDump { task, now });
            sys_post(
                sys_refresh_task_id(self.jeff),
                notifications::jeffrey::DUMP_REQUEST_MASK,
            );
        } else {
            // in practice, this should never happen...but let's not generate
            // panic code for it.
            ringbuf_entry!(Trace::DumpQueueFull);
        }
    }

    pub(super) fn start_dump(&mut self) -> Option<u32> {
        let now = sys_get_timer().now;
        if let Some(task) = self.queue.front().copied() {
            ringbuf_entry!(Trace::StartDump { task, now });
            Some(task)
        } else {
            ringbuf_entry!(Trace::UnexpectedDumpReq);
            None
        }
    }

    pub(super) fn finish_dump(
        &mut self,
        task: u32,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        let now = sys_get_timer().now;
        ringbuf_entry!(Trace::DumpFinished { task, now });

        // Pop the queued task ID, and make sure it was the one we expected.
        if self.queue.pop_front() != Some(task) {
            // Jeffrey screwed up somehow! Let's kill him.
            return Err(idol_runtime::ClientError::BadMessageContents.fail());
        }

        // If there's more tasks in need of dumping, let Jeffrey know he has
        // more work to do.
        if !self.queue.is_empty() {
            sys_post(
                sys_refresh_task_id(self.jeff),
                notifications::jeffrey::DUMP_REQUEST_MASK,
            );
        }

        Ok(())
    }
}
