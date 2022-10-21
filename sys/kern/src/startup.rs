// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel startup.

use crate::atomic::AtomicExt;
use crate::task::Task;
use crate::uninit;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

/// Tracks when a mutable reference to the task table is floating around in
/// kernel code, to prevent production of a second one. This forms a sort of
/// ad-hoc Mutex around the task table.
///
/// Notice that this begins life initialized to `true`. This prevents use of
/// `with_task_table` et al before the kernel is properly started. We set it to
/// `false` late in `start_kernel`.
static TASK_TABLE_IN_USE: AtomicBool = AtomicBool::new(true);

pub const HUBRIS_FAULT_NOTIFICATION: u32 = 1;

/// The main kernel entry point.
///
/// We currently expect an application to provide its own `main`-equivalent
/// function, which does basic hardware setup and then calls this function.
///
/// Parameters:
///
/// - `tick_divisor`: a platform-specific way of converting "machine ticks" into
///   "kernel ticks." On ARM M-profile, this is CPU cycles per tick, where a
///   tick is typically a millisecond.
///
/// # Safety
///
/// This function has architecture-specific requirements for safe use -- on ARM,
/// for instance, it must be called from the main (interrupt) stack in
/// privileged mode.
///
/// This function may not be called reentrantly or from multiple cores.
pub unsafe fn start_kernel(tick_divisor: u32) -> ! {
    // Set our clock frequency so debuggers can find it as needed
    //
    // Safety: TODO it is not clear that this operation needs to be unsafe.
    unsafe {
        crate::arch::set_clock_freq(tick_divisor);
    }

    // Grab references to all our statics.
    let task_descs = &HUBRIS_TASK_DESCS;
    let region_descs = &HUBRIS_REGION_DESCS;
    // Safety: these references will remain unique so long as the "only called
    // once per boot" contract on this function is upheld.
    let (task_table, region_tables) = unsafe {
        (&mut HUBRIS_TASK_TABLE_SPACE, &mut HUBRIS_REGION_TABLE_SPACE)
    };

    // Initialize our RAM data structures.

    // We currently just refer to the RegionDescs in Flash. No additional
    // preparation of those structures is required here. This will almost
    // certainly need to change in the future: we can save many cycles by (1)
    // storing them in an architecture-optimized format for this particular MPU,
    // and (2) moving them into RAM where random accesses don't imply wait
    // states.

    // As a small optimization, we equip each task with an array of references
    // to RegionDescs, instead of looking them up by index each time. Generate
    // these. The build script has given us HUBRIS_REGION_TABLE_SPACE, which has
    // type
    //
    // MaybeUninit<[[&RegionDesc; REGIONS_PER_TASK]; HUBRIS_TASK_COUNT]>
    //
    // which we can transform into
    //
    // [[MaybeUninit<&RegionDesc>; REGIONS_PER_TASK; HUBRIS_TASK_COUNT]
    //
    // through two uninit::unbundle steps:
    let region_tables = uninit::unbundle(uninit::unbundle(region_tables));

    for (i, table) in region_tables.iter_mut().enumerate() {
        for (slot, &index) in table.iter_mut().zip(&task_descs[i].regions) {
            slot.write(&region_descs[index as usize]);
        }
    }

    // Safety: we have fully initialized this and can shed the uninit part.
    // We're also dropping &mut.
    let region_tables = unsafe { uninit::assume_init_ref(region_tables) };

    // Now, generate the task table.
    let task_table = uninit::unbundle(task_table);
    for (i, task) in task_table.iter_mut().enumerate() {
        task.write(Task::from_descriptor(
            &task_descs[i],
            &region_tables[i],
        ));
    }

    // Safety: we have fully initialized this and can shed the uninit part.
    let task_table = unsafe { uninit::assume_init_mut(task_table) };

    // With that done, set up initial register state etc.
    for task in task_table.iter_mut() {
        crate::arch::reinitialize(task);
    }

    // Great! Pick our first task. We'll act like we're scheduling after the
    // last task, which will cause a scan from 0 on.
    let first_task_index =
        crate::task::select(task_table.len() - 1, task_table);

    crate::arch::apply_memory_protection(&task_table[first_task_index]);
    TASK_TABLE_IN_USE.store(false, Ordering::Release);
    crate::arch::start_first_task(
        tick_divisor,
        &mut task_table[first_task_index],
    )
}

/// Runs `body` with a reference to the task table.
///
/// To preserve uniqueness of the `&mut` reference passed into `body`, this
/// function will detect any attempts to call it recursively and panic.
pub(crate) fn with_task_table<R>(body: impl FnOnce(&mut [Task]) -> R) -> R {
    if TASK_TABLE_IN_USE.swap_polyfill(true, Ordering::Acquire) {
        panic!(); // recursive use of with_task_table
    }
    // Safety: we have observed `TASK_TABLE_IN_USE` being false, which means the
    // task table is initialized (note that at reset it starts out true) and
    // that we're not already within a call to with_task_table. Thus, we can
    // produce a reference to the task table without aliasing.
    let task_table = unsafe { &mut HUBRIS_TASK_TABLE_SPACE };

    // Rearrange the braces in the type to get an array of potentially uninit
    let task_table = uninit::unbundle(task_table);

    // Safety: because `TASK_TABLE_IN_USE` is false, we know it is initialized.
    let task_table = unsafe { uninit::assume_init_mut(task_table) };

    let r = body(task_table);

    TASK_TABLE_IN_USE.store(false, Ordering::Release);

    r
}

include!(concat!(env!("OUT_DIR"), "/kconfig.rs"));
