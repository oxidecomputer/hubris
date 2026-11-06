// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// Macro for obtaining at task-slot's index mapping.
///
/// The index of each task slot from the app manifest is provided by
/// `xtask` as an environment variable when compiling, using the form
/// `HUBRIS_TASK_SLOT_$task=$index`.
#[macro_export]
macro_rules! task_slot {
    ($vis:vis $var:ident, $task_name:ident) => {
        $vis static $var: $crate::task_slot::TaskSlot = {
            // Gets the task index (as a str) for a given slot, erroring
            // at compile time if not defined.
            const TASK_IDX_STR: &str = env!(
                // env var name, e.g. `HUBRIS_TASK_SLOT_$task_name`
                concat!("HUBRIS_TASK_SLOT_", stringify!($task_name)),
                // error emitted if env var is not defined
                concat!(
                    "task slot `",
                    stringify!($task_name),
                    "` is not bound for this task; add it to the task's",
                    " `task-slots` in the app.toml",
                ),
            );

            // Use `TaskSlot::parse` to turn the "10" from the example above
            // into a `TaskSlot(10)`.
            $crate::task_slot::TaskSlot::parse(TASK_IDX_STR)
        };
    };
}
