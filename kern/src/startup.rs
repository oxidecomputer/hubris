//! Kernel startup.

use crate::app;
use crate::task::{SchedState, Task, TaskState};

pub unsafe fn start_kernel(
    app_header_ptr: *const app::App,
    scratch_ram: *mut u8,
    scratch_ram_size: usize,
) -> ! {
    // Create our simple allocator.
    let alloc = BumpPointer(core::slice::from_raw_parts_mut(
        scratch_ram,
        scratch_ram_size,
    ));
    // Validate the app header!
    let app_header = &*app_header_ptr;
    assert_eq!(app_header.magic, app::CURRENT_APP_MAGIC);
    // TODO task count less than some configured maximum

    // We use 8-bit region numbers in task descriptions, so we have to limit the
    // number of defined regions.
    assert!(app_header.region_count < 256);

    // Check that no mysterious data appears in the reserved space.
    assert_eq!(app_header.zeroed_expansion_space, [0; 20]);

    // Derive the addresses of the other regions from the app header.
    let tasks_ptr = app_header_ptr.offset(1) as *const app::TaskDesc;
    let tasks =
        core::slice::from_raw_parts(tasks_ptr, app_header.task_count as usize);

    let regions = core::slice::from_raw_parts(
        tasks_ptr.offset(app_header.task_count as isize)
            as *const app::RegionDesc,
        app_header.region_count as usize,
    );

    // Validate regions first, since tasks will use them.
    for region in regions {
        // Check for use of reserved attributes.
        assert!(!region
            .attributes
            .intersects(app::RegionAttributes::RESERVED));
        // Check for base+size overflow
        assert!(region.base.wrapping_add(region.size) >= region.base);
        // Check for suspicious use of reserved word
        assert_eq!(region.reserved_zero, 0);
    }

    // Validate tasks next.
    for task in tasks {
        assert!(!task.flags.intersects(app::TaskFlags::RESERVED));

        let mut entry_pt_found = false;
        let mut stack_ptr_found = false;
        for &region_idx in &task.regions {
            assert!(region_idx < app_header.region_count as u8);
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

        assert!(entry_pt_found);
        assert!(stack_ptr_found);
    }

    // Okay, we're pretty sure this is all legitimate.
    safe_start_kernel(app_header, tasks, regions, alloc)
}

fn safe_start_kernel(
    app_header: &'static app::App,
    task_descs: &'static [app::TaskDesc],
    region_descs: &'static [app::RegionDesc],
    mut alloc: BumpPointer,
) -> ! {
    // Allocate our RAM data
    // structures. First, the task table.
    let tasks = alloc.gimme_n(app_header.task_count as usize, |i| {
        let task_desc = &task_descs[i];
        Task {
            priority: task_desc.priority,
            state: if task_desc.flags.contains(app::TaskFlags::START_AT_BOOT) {
                TaskState::Healthy(SchedState::Runnable)
            } else {
                TaskState::default()
            },
            ..Task::default()
        }
    });
    // Now, allocate a region table for each task, turning its ROM indices into
    // pointers. Note: if we decide to convert the RegionDesc into an
    // architecture-specific optimized form, that would happen here instead.
    for (task, task_desc) in tasks.iter_mut().zip(task_descs) {
        task.region_table = alloc.gimme_n(app::REGIONS_PER_TASK, |i| {
            &region_descs[task_desc.regions[i] as usize]
        });
    }

    // Great! Pick our first task. We're looking for the runnable-at-boot task
    // with the numerically-lowest priority. If there's more than one, we'll
    // arbitrarily choose...the first!
    let mut chosen_task = None;
    for (i, task) in tasks.iter().enumerate() {
        if task.state == TaskState::Healthy(SchedState::Runnable) {
            if let Some((_, current_prio)) = chosen_task {
                if current_prio <= task.priority {
                    continue;
                }
            }
            chosen_task = Some((i, task.priority));
        }
    }
    let (first_task_index, _) = chosen_task.expect("no runnable tasks");

    switch_to_user(tasks, first_task_index)
}

fn switch_to_user(_tasks: &mut [Task], _first_task_index: usize) -> ! {
    panic!()
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
