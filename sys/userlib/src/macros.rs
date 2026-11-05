// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// Macro for obtaining task slots.
///
/// This mapping is provided as an environment variable at build time
/// by `xtask`, via `HUBRIS_TASK_SLOT_$task` mappings.
#[macro_export]
macro_rules! task_slot {
    ($vis:vis $var:ident, $task_name:ident) => {
        $vis static $var: $crate::task_slot::TaskSlot =
            $crate::task_slot::TaskSlot::new(env!(
                concat!("HUBRIS_TASK_SLOT_", stringify!($task_name)),
                concat!(
                    "task slot `",
                    stringify!($task_name),
                    "` is not bound for this task; add it to the task's \
                     `task-slots` in the app.toml",
                ),
            ));
    };
}
