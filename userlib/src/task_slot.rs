use abi::{Generation, TaskId};

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
pub struct TaskSlot(u16);

impl TaskSlot {
    /// A TaskSlot that has not been resolved by a later processing step.
    ///
    /// Calling get_task_id() on an unbound TaskSlot will panic.
    pub const UNBOUND: Self = Self(TaskId::UNBOUND.0);

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
        // In the expected use case of a static TaskSlot instance, such an
        // instance is considered immutable by the compiler.  The compiler may
        // choose to exploit that immutability to constant-fold the value of
        // self.0 (as known at compile-time).  Since the intent of TaskSlot is
        // to modify the value of self.0 after compilation, it does not meet the
        // compiler's definition of immutable but the compiler doesn't know that
        // without some help.  This is similar to interior mutability a la
        // UnsafeCell but slightly different as a TaskSlot instance _is_
        // immutable at runtime and there is only ever one reference to self.0.
        // Instead, we need to force the compiler to load self.0 from RAM as it
        // couldn't know what the value is at compile time.  This is effectively
        // the same as a volatile read where the value in storage may be changed
        // outside the compiler and runtime.
        unsafe { core::ptr::read_volatile(&self.0) }
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
            taskidx_address: &task_slot.0,
            slot_name_len: slot_name.len(),
            slot_name: *slot_name,
        }
    }
}

// SAFETY
//
// Storing a pointer in a struct causes it to not implement Sync automatically.
// In this case, PeerTaskTableEntry can only be constructed via for_peer_task()
// which requires &'static arguments.  Thus, the stored pointer can only be to a
// static PeerTask.  Further, for_peer_task() is only intended to be used by
// peer_task!() which places the PeerTaskTableEntry in a .peer_task_table linker
// section that is treated similar to debug information in that no virtual
// addresses are allocated to the contents and the section is not loaded into
// the process space.  As such, instances of PeerTaskTableEntry will never exist
// at runtime.
unsafe impl<const N: usize> Sync for TaskSlotTableEntry<N> {}
