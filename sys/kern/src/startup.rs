// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel startup.

use crate::app;
use crate::task::{self, Task};

/// The main kernel entry point.
///
/// We currently expect an application to provide its own `main`-equivalent
/// function, which does basic hardware setup and then calls this function with
/// the location of the `App` header and some kernel-dedicated RAM.
///
/// Parameters:
///
/// - `app_header_ptr` is the address of the application header, found through
///   whatever eldritch magic you choose (probably a linker symbol).
/// - `scratch_ram` and `scratch_ram_size` are the base and extent of a section
///   of bytes that the kernel will use for its own purposes. Its required size
///   depends on the number of tasks you allocate. (TODO: we should give more
///   guidance than that.) It's important for correctness that this *not* be
///   accessible to any task.
/// - `tick_divisor`: a platform-specific way of converting "machine ticks" into
///   "kernel ticks." On ARM M-profile, this is CPU cycles per tick, where a
///   tick is typically a millisecond.
///
/// # Safety
///
/// This can be called exactly once per boot, with valid pointers that don't
/// alias any other structure or one another.
pub unsafe fn start_kernel(
    app_header_ptr: *const app::App,
    scratch_ram: *mut u8,
    scratch_ram_size: usize,
    tick_divisor: u32,
) -> ! {
    klog!("starting: laziness");

    // Create our simple allocator.
    let alloc = BumpPointer(core::slice::from_raw_parts_mut(
        scratch_ram,
        scratch_ram_size,
    ));
    // Validate the app header!
    let app_header = &*app_header_ptr;
    uassert_eq!(app_header.magic, app::CURRENT_APP_MAGIC);
    // TODO task count less than some configured maximum

    // We use 8-bit region numbers in task descriptions, so we have to limit the
    // number of defined regions.
    uassert!(app_header.region_count < 256);

    // Check that no mysterious data appears in the reserved space.
    uassert_eq!(app_header.zeroed_expansion_space, [0; 12]);

    // Derive the addresses of the other regions from the app header.
    // Regions come first.
    let regions_ptr = app_header_ptr.offset(1) as *const app::RegionDesc;
    let regions = core::slice::from_raw_parts(
        regions_ptr,
        app_header.region_count as usize,
    );

    let tasks_ptr = regions_ptr.offset(app_header.region_count as isize)
        as *const app::TaskDesc;
    let tasks =
        core::slice::from_raw_parts(tasks_ptr, app_header.task_count as usize);

    let interrupts = core::slice::from_raw_parts(
        tasks_ptr.offset(app_header.task_count as isize)
            as *const app::Interrupt,
        app_header.irq_count as usize,
    );

    // Validate regions first, since tasks will use them.
    for region in regions {
        // Check for use of reserved attributes.
        uassert!(!region
            .attributes
            .intersects(app::RegionAttributes::RESERVED));
        // Check for base+size overflow
        uassert!(region.base.checked_add(region.size).is_some());
        // Check for suspicious use of reserved word
        uassert_eq!(region.reserved_zero, 0);

        #[cfg(armv7m)]
        uassert!(region.size.is_power_of_two());
    }

    // Validate tasks next.
    for task in tasks {
        uassert!(!task.flags.intersects(app::TaskFlags::RESERVED));

        let mut entry_pt_found = false;
        let mut stack_ptr_found = false;
        for &region_idx in &task.regions {
            uassert!(region_idx < app_header.region_count as u8);
            let region = &regions[region_idx as usize];
            if task.entry_point.wrapping_sub(region.base) < region.size {
                if region.attributes.contains(app::RegionAttributes::EXECUTE) {
                    entry_pt_found = true;
                }
            }
            // Note that stack pointer is compared using <=, because it's okay
            // to have it point just off the end as the stack is initially
            // empty.
            if task.initial_stack.wrapping_sub(region.base) <= region.size {
                if region.attributes.contains(
                    app::RegionAttributes::READ | app::RegionAttributes::WRITE,
                ) {
                    stack_ptr_found = true;
                }
            }
        }

        uassert!(entry_pt_found);
        uassert!(stack_ptr_found);
    }

    // Finally, check interrupts.
    for irq in interrupts {
        // Valid task index?
        uassert!(irq.task < tasks.len() as u32);
    }

    // Okay, we're pretty sure this is all legitimate.
    safe_start_kernel(
        app_header,
        tasks,
        regions,
        interrupts,
        alloc,
        tick_divisor,
    )
}

fn safe_start_kernel(
    app_header: &'static app::App,
    task_descs: &'static [app::TaskDesc],
    region_descs: &'static [app::RegionDesc],
    interrupts: &'static [app::Interrupt],
    mut alloc: BumpPointer,
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
    let region_tables = alloc.gimme_n(app_header.task_count as usize, |i| {
        let mut table = [&region_descs[0]; app::REGIONS_PER_TASK];
        for (slot, &index) in table.iter_mut().zip(&task_descs[i].regions) {
            *slot = &region_descs[index as usize];
        }
        table
    });
    // We don't need further mut access
    let region_tables = &region_tables[..];

    // Now, generate the task table.
    let tasks = alloc.gimme_n(app_header.task_count as usize, |i| {
        Task::from_descriptor(&task_descs[i], &region_tables[i])
    });

    uassert!(tasks.len() != 0); // tasks must exist for this to work.

    // With that done, set up initial register state etc.
    for task in tasks.iter_mut() {
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
        crate::arch::set_task_table(tasks);
        crate::arch::set_irq_table(interrupts);
    }
    task::set_fault_notification(app_header.fault_notification);

    // Great! Pick our first task. We'll act like we're scheduling after the
    // last task, which will cause a scan from 0 on.
    let first_task_index = crate::task::select(tasks.len() - 1, tasks);

    crate::arch::apply_memory_protection(&tasks[first_task_index]);
    klog!("starting: hubris");
    crate::arch::start_first_task(tick_divisor, &tasks[first_task_index])
}

struct BumpPointer(&'static mut [u8]);

impl BumpPointer {
    pub fn gimme_n<T>(
        &mut self,
        n: usize,
        mut init: impl FnMut(usize) -> T,
    ) -> &'static mut [T] {
        use core::mem::{align_of, size_of};

        // Temporarily steal the entire allocation region from self. This helps
        // with lifetime inference issues.
        let free = core::mem::replace(&mut self.0, &mut []);

        // Bump the pointer up to the required alignment for T.
        let align_delta = free.as_ptr().align_offset(align_of::<T>());
        let (_discarded, free) = free.split_at_mut(align_delta);
        // Split off RAM for a T.
        let (allocated, free) = free.split_at_mut(n * size_of::<T>());

        // Put free memory back.
        self.0 = free;

        // `allocated` has the alignment and size of a `T`, so we can start
        // treating it like one. However, we have to initialize it first --
        // without dropping its current contents!
        let allocated = allocated.as_mut_ptr() as *mut T;
        for i in 0..n {
            unsafe {
                allocated.add(i).write(init(i));
            }
        }
        unsafe { core::slice::from_raw_parts_mut(allocated, n) }
    }

    #[allow(dead_code)] // TODO: if we really don't need this, remove it
    pub fn gimme<T>(&mut self, value: T) -> &'static mut T {
        use core::mem::{align_of, size_of};

        // Temporarily steal the entire allocation region from self. This helps
        // with lifetime inference issues.
        let free = core::mem::replace(&mut self.0, &mut []);

        // Bump the pointer up to the required alignment for T.
        let align_delta = free.as_ptr().align_offset(align_of::<T>());
        let (_discarded, free) = free.split_at_mut(align_delta);
        // Split off RAM for a T.
        let (allocated, free) = free.split_at_mut(size_of::<T>());

        // Put free memory back.
        self.0 = free;

        // `allocated` has the alignment and size of a `T`, so we can start
        // treating it like one. However, we have to initialize it first --
        // without dropping its current contents!
        let allocated = allocated.as_mut_ptr() as *mut T;
        unsafe {
            allocated.write(value);
            &mut *allocated
        }
    }
}
