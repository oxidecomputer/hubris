// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use abi::{Generation, TaskId};

/// Host version of the task slot: there is no post-link step, so the slot
/// keeps its name and asks the fixture for the task index the first time it
/// is used.
pub struct TaskSlot {
    name: &'static str,
    index: std::sync::OnceLock<u16>,
}

impl TaskSlot {
    pub const fn named(name: &'static str) -> Self {
        Self {
            name,
            index: std::sync::OnceLock::new(),
        }
    }

    pub fn get_task_id(&self) -> TaskId {
        let task_index = self.get_task_index();

        let prototype =
            TaskId::for_index_and_gen(task_index.into(), Generation::default());
        crate::sys_refresh_task_id(prototype)
    }

    pub fn get_task_index(&self) -> u16 {
        *self.index.get_or_init(|| {
            hostcall::task_slot(self.name).unwrap_or_else(|| {
                panic!(
                    "task slot {:?} is not defined in {}",
                    self.name,
                    hostcall::TASK_SLOTS_VAR
                )
            })
        })
    }
}
