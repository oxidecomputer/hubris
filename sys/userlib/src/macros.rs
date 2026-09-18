// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub use bstringify;
pub use paste;

/// Declares a task slot named `$task_name`, bound to a `TaskSlot` static
/// called `$var`.
///
/// On a Hubris target the slot is a placeholder in `.rodata`, listed in the
/// `.task_slot_table` section so the build system can fill in the task index
/// after linking. On the host it records the name, and the fixture resolves it
/// on first use.
#[macro_export]
macro_rules! task_slot {
    ($vis:vis $var:ident, $task_name:ident) => {
        $crate::macros::paste::paste! {
            #[cfg(target_os = "none")]
            #[used]
            $vis static $var: $crate::task_slot::TaskSlot =
                $crate::task_slot::TaskSlot::UNBOUND;

            #[cfg(target_os = "none")]
            #[used]
            #[unsafe(link_section = ".task_slot_table")]
            static [< _TASK_SLOT_TABLE_ $var >]: $crate::task_slot::TaskSlotTableEntry<
                { $crate::macros::bstringify::bstringify!($task_name).len() },
            > = $crate::task_slot::TaskSlotTableEntry::for_task_slot(
                $crate::macros::bstringify::bstringify!($task_name),
                &$var,
            );

            #[cfg(not(target_os = "none"))]
            $vis static $var: $crate::task_slot::TaskSlot =
                $crate::task_slot::TaskSlot::named(stringify!($task_name));
        }
    };
}
