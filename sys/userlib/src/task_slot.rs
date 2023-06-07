// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use abi::{Generation, TaskId};
use volatile_const::VolatileConst;

/// Placeholder for post-compilation linking of tasks.
///
/// Most tasks will need to interact with other tasks by sending messages to
/// their TaskIds. In many cases, the task where the required functionality is
/// implemented will vary based on the application. Since the exact target task
/// may not be known at task compile time but will be known later, task slots
/// are used to create compile-time placeholders that are filled in with a
/// task's identifying information by a post-compile process.  These
/// placeholders can then be converted into TaskId at runtime.
#[repr(C)]
pub struct TaskSlot(VolatileConst<u16>);

impl TaskSlot {
    /// A TaskSlot that has not been resolved by a later processing step.
    ///
    /// Calling get_task_id() on an unbound TaskSlot will panic.
    pub const UNBOUND: Self = Self(VolatileConst::new(TaskId::UNBOUND.0));

    pub fn get_task_id(&self) -> TaskId {
        let task_index = self.get_task_index();

        if task_index == TaskId::UNBOUND.0 {
            panic!("Attempted to get task id of unbound TaskSlot");
        }

        let prototype =
            TaskId::for_index_and_gen(task_index.into(), Generation::default());
        crate::sys_refresh_task_id(prototype)
    }

    pub fn get_task_index(&self) -> u16 {
        self.0.get()
    }
}

/// Description of a task slot in .task_slot_table ELF section.
///
/// Most tasks will need to interact with other tasks by sending messages to
/// their TaskIds. In many cases, the task where the required functionality is
/// required will vary based on the application. Since the exact target task may
/// not be known at task compile time but will be known later, task slots are
/// used to create compile-time placeholders that are filled in with a task's
/// identifying information by a post-compile process.  These placeholders can
/// then be converted into TaskId at runtime.
///
/// Each task slot entry consist of an ASCII name and the address of the task
/// slot's placeholder in the task's binary. While not part of the kernel/task
/// ABI, these entries are part of the task's ABI that is used by the build
/// system.
#[repr(C)]
#[repr(packed)]
pub struct TaskSlotTableEntry<const N: usize> {
    taskidx_address: *const u16,
    slot_name_len: usize,
    slot_name: [u8; N],
}

impl<const N: usize> TaskSlotTableEntry<N> {
    pub const fn for_task_slot(
        slot_name: &'static [u8; N],
        task_slot: &'static TaskSlot,
    ) -> Self {
        Self {
            // Directly reading through the pointer returned by
            // VolatileConst::as_ptr() is always an error.  In this case,
            // for_task_slot() is only intended to be used by task_slot!() which
            // places the TaskSlotTableEntry in a .task_slot_table linker
            // section that is treated similar to debug information in that no
            // virtual addresses are allocated to the contents and the section
            // is not loaded into the process space.  As such, instances of
            // TaskSlotTableEntry will never exist at runtime and thus the
            // pointer will never be read through at runtime.
            taskidx_address: task_slot.0.as_ptr(),
            slot_name_len: slot_name.len(),
            slot_name: *slot_name,
        }
    }
}

// SAFETY
//
// Storing a pointer in a struct causes it to not implement Sync automatically.
// In this case, TaskSlotTableEntry can only be constructed via for_task_slot()
// which requires &'static arguments.  Thus, the stored pointer can only be to a
// static TaskSlot.  Further, for_task_slot() is only intended to be used by
// task_slot!() which places the TaskSlotTableEntry in a .task_slot_table linker
// section that is treated similar to debug information in that no virtual
// addresses are allocated to the contents and the section is not loaded into
// the process space.  As such, instances of TaskSlotTableEntry will never exist
// at runtime.
unsafe impl<const N: usize> Sync for TaskSlotTableEntry<N> {}
