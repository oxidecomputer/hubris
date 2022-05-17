// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel startup.

use crate::app;
use crate::task::Task;
use core::mem::MaybeUninit;

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
/// This can be called exactly once per boot.
pub unsafe fn start_kernel(tick_divisor: u32) -> ! {
    klog!("starting: laziness");

    // Set our clock frequency so debuggers can find it as needed
    crate::arch::set_clock_freq(tick_divisor);

    // Grab references to all our statics and start the safe code.
    safe_start_kernel(
        &HUBRIS_TASK_DESCS,
        &HUBRIS_REGION_DESCS,
        &mut HUBRIS_TASK_TABLE_SPACE,
        &mut HUBRIS_REGION_TABLE_SPACE,
        tick_divisor,
    )
}

fn safe_start_kernel(
    task_descs: &'static [app::TaskDesc],
    region_descs: &'static [app::RegionDesc],
    task_table: &'static mut MaybeUninit<[Task; HUBRIS_TASK_COUNT]>,
    region_tables: &'static mut MaybeUninit<
        [[&'static app::RegionDesc; app::REGIONS_PER_TASK]; HUBRIS_TASK_COUNT],
    >,
    tick_divisor: u32,
) -> ! {
    klog!("starting: impatience");

    // Allocate our RAM data structures.

    // We currently just refer to the RegionDescs in Flash. No additional
    // preparation of those structures is required here. This will almost
    // certainly need to change in the future: we can save many cycles by (1)
    // storing them in an architecture-optimized format for this particular MPU,
    // and (2) moving them into RAM where random accesses don't imply wait
    // states.

    // As a small optimization, we equip each task with an array of references
    // to RegionDecs, instead of looking them up by index each time. Generate
    // these.

    // Safety: MaybeUninit<[T]> -> [MaybeUninit<T>] is defined as safe.
    let region_tables: &mut [[MaybeUninit<&'static app::RegionDesc>; app::REGIONS_PER_TASK];
             HUBRIS_TASK_COUNT] =
        unsafe { &mut *(region_tables as *mut _ as *mut _) };

    for (i, table) in region_tables.iter_mut().enumerate() {
        for (slot, &index) in table.iter_mut().zip(&task_descs[i].regions) {
            *slot = MaybeUninit::new(&region_descs[index as usize]);
        }
    }

    // Safety: we have fully initialized this and can shed the uninit part.
    // We're also dropping &mut.
    let region_tables: &[[&'static app::RegionDesc; app::REGIONS_PER_TASK];
         HUBRIS_TASK_COUNT] = unsafe { &*(region_tables as *mut _ as *mut _) };

    // Now, generate the task table.
    // Safety: MaybeUninit<[T]> -> [MaybeUninit<T>] is defined as safe.
    let task_table: &mut [MaybeUninit<Task>; HUBRIS_TASK_COUNT] =
        unsafe { &mut *(task_table as *mut _ as *mut _) };
    for (i, task) in task_table.iter_mut().enumerate() {
        *task = MaybeUninit::new(Task::from_descriptor(
            &task_descs[i],
            &region_tables[i],
        ));
    }

    // Safety: we have fully initialized this and can shed the uninit part.
    let task_table: &mut [Task; HUBRIS_TASK_COUNT] =
        unsafe { &mut *(task_table as *mut _ as *mut _) };

    // With that done, set up initial register state etc.
    for task in task_table.iter_mut() {
        crate::arch::reinitialize(task);
    }

    // Stash the table extents somewhere that we can get it later, cheaply,
    // without recomputing stuff. This is treated as architecture specific
    // largely as a nod to simulators that might want to use a thread local
    // rather than a global static, but some future pleasant architecture might
    // let us store this in secret registers...
    //
    // Safety: as long as we don't call `with_task_table` or `with_irq_table`
    // after this point before switching to user, we can't alias, and we'll be
    // okay.
    unsafe {
        // TODO: these could be done by the linker...
        crate::arch::set_task_table(task_table);
    }

    // Great! Pick our first task. We'll act like we're scheduling after the
    // last task, which will cause a scan from 0 on.
    let first_task_index =
        crate::task::select(task_table.len() - 1, task_table);

    crate::arch::apply_memory_protection(&task_table[first_task_index]);
    klog!("starting: hubris");
    crate::arch::start_first_task(tick_divisor, &task_table[first_task_index])
}

include!(concat!(env!("OUT_DIR"), "/kconfig.rs"));
