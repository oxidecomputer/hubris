// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub use bstringify;
pub use paste;

#[macro_export]
macro_rules! task_slot {
    ($vis:vis $var:ident, $task_name:ident) => {
        $crate::macros::paste::paste! {
            #[used]
            $vis static $var: $crate::task_slot::TaskSlot =
                $crate::task_slot::TaskSlot::UNBOUND;

            #[used]
            #[link_section = ".task_slot_table"]
            static [< _TASK_SLOT_TABLE_ $var >]: $crate::task_slot::TaskSlotTableEntry<
                { $crate::macros::bstringify::bstringify!($task_name).len() },
            > = $crate::task_slot::TaskSlotTableEntry::for_task_slot(
                $crate::macros::bstringify::bstringify!($task_name),
                &$var,
            );
        }
    };
}
