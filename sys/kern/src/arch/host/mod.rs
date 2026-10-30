// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Host Arch impl
//!
//! This Arch impl runs as a host process, and spawns tasks as separate host
//! processes. The [`hostcall`] crate is used as a protocol between the kernel
//! and tasks, using the task's stdin/stdout for communication.
//!
//! Normally during system calls, the task would provide read/write access to
//! certain buffers (input, output, leases), allowing the kernel (or other tasks
//! in the case of leases) to access this memory. Instead, the task serializes
//! the full contents of these buffers when messaging the kernel, and the kernel
//! stores these deserialized buffers locally. We then use these stored buffers
//! as a "proxy": writes happen locally, and are serialized back to the task
//! when the syscall returns.
//!
//! Despite operating as multiple OS processes, the control flow between the
//! kernel and tasks is still lock-step (emulating single threaded behavior),
//! as all entities except for the currently running one will block waiting for
//! a reply from the currently running entity.
//!
//! When a task yields back to the kernel (making a syscall), the kernel
//! performs regular scheduling behaviors, which then decides which task will
//! be resumed when the kernel yields.

use std::collections::BTreeSet;
use std::fmt::Display;
use std::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use abi::{InterruptNum, IrqStatus, TaskId, UsageError};

use crate::arch::Arch;
use crate::atomic::AtomicExt;
use crate::descs::RegionAttributes;
use crate::startup::with_task_table;
use crate::task::{self, NotificationSet, Task};
use crate::time::Timestamp;

/// The kernel's tracing output
#[macro_use]
mod trace;

/// Configuration loaded from environment variables or files
mod config;

/// Interactions with task processes
mod host_task;

use config::config;
use host_task::{Call, SavedState, processes, spawn_task};
use trace::{describe_syscall, syscall_name, tracing};

/// Something has gone terribly wrong, and it is time for the kernel to stop.
fn fatal(msg: impl Display) -> ! {
    eprintln!("kernel: {msg}");
    std::process::exit(2)
}

/// Ends the run normally: the simulated system has nothing left to do.
fn finish(msg: impl Display) -> ! {
    eprintln!("kernel: stopping: {msg}");
    std::process::exit(0)
}

/// Get the name of the task based on its index.
fn task_name_from_idx(index: usize) -> &'static str {
    match config().tasks.get(index) {
        Some(task) => &task.name,
        None if index == TaskId::KERNEL.index() => "kernel",
        None => "?",
    }
}

/////////////////////////////////////////////////////////////////////////////
// The scheduler loop

/// The currently scheduled task.
static CURRENT_TASK_PTR: AtomicPtr<Task> =
    AtomicPtr::new(core::ptr::null_mut());

/// Virtual time, in ticks (unused in real-time mode).
static TICKS: AtomicU64 = AtomicU64::new(0);

/// When the kernel started, for real-time mode.
static START: OnceLock<Instant> = OnceLock::new();

/// Software interrupts pended by `pend_software_irq`, delivered at the next
/// scheduling point.
static PENDING_IRQS: Mutex<Vec<InterruptNum>> = Mutex::new(Vec::new());

/// Interrupts a task has enabled, so `irq_status` can report them.
static ENABLED_IRQS: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

fn current_task_index() -> (*mut Task, usize) {
    let current = CURRENT_TASK_PTR.load(Ordering::Relaxed);
    if current.is_null() {
        fatal("no current task");
    }
    // Safety: the pointer came from a `&mut Task` in the task table (see
    // `set_current_task`), and this is only called between kernel entries,
    // when no other reference into the table is live.
    let index = unsafe { usize::from((*current).descriptor().index) };
    (current, index)
}

/// One trip through the scheduler: resume the current task, wait for its
/// next syscall, and run the kernel for it.
fn step() {
    let (current, index) = current_task_index();
    let cfg = config();

    // Calculate task name width. This is a little extra, but it also seemed
    // wasteful to me to iterate the list of tasks and calculate the max name
    // width on every iteration.
    let width = {
        static MAX_WIDTH: AtomicUsize = AtomicUsize::new(0);
        let mut width = MAX_WIDTH.load(Ordering::Relaxed);

        // Use 0 as a sentinel. If we observe this as zero, find the longest
        // task name so we can print the tasks neatly when tracing. If there
        // are no tasks, or all tasks have a zero length name (???), then just
        // report the max width as 1 to avoid re-calculating.
        if width == 0 {
            width = cfg
                .tasks
                .iter()
                .map(|t| t.name.as_str().len())
                .max()
                .unwrap_or(0)
                .max(1);
            MAX_WIDTH.store(width, Ordering::Relaxed);
        }

        width
    };

    // If this is an "idle" task, we're not going to call into it and return,
    // just do the appropriate book-keeping, and return from the current step.
    if cfg.tasks.get(index).is_some_and(|t| t.idle) {
        idle_step(index, width);
        return;
    }

    // If this isn't an idle task, and the kernel has decided to schedule this
    // task, then we either have something to tell it, or the task has just
    // been (re-)spawned.

    // Interact with the external task process
    let res = {
        // Get the process handle we are about to interact with
        let mut procs = processes();
        let Some(proc) = procs.get_mut(index).and_then(Option::as_mut) else {
            fatal(format_args!("task {index} has no process to resume"))
        };

        // Obtain the finished task call, if any.
        //
        // Safety: as in `current_task_index`; the reference is dropped before
        // any other kernel entry below.
        let finished_call = unsafe { (*current).save_mut().take_call() };

        // Trace a bit about what we're about to send to this task.
        finished_call.as_ref().inspect(|call| {
            trace_task!(
                &[index],
                "task {:>width$}: resumed from {} with {:02X?}",
                cfg.tasks[index].name,
                syscall_name(call.syscall_nr),
                call.rets
            );
        });

        // Schedule into the task, giving it the response if there is one.
        proc.resume(index, finished_call)
    };

    // The task has now yielded back to us. Let's see how that interaction went,
    // and what we need to do about it.
    match res {
        Ok(call) => {
            // TODO(AJM): We only do the `tracing()` call here to avoid the
            // describe and participants stuff, `trace_task!` already has a
            // check for this. We can probably make this less awkward.
            if tracing() {
                let (what, peer) = describe_syscall(&call);
                let participants: Vec<usize> =
                    [Some(index), peer].into_iter().flatten().collect();
                trace_task!(
                    &participants,
                    "task {:>width$}: {what}",
                    cfg.tasks[index].name
                );
            }

            // The interaction was successful! We gave the task our [`Return`],
            // and it gave us a new [`Syscall`] (which we've converted into a
            // [`Call`]). Let's store that off, and then tell the kernel that
            // there is a new syscall to handle.

            let nr = call.syscall_nr;
            // Safety: as in `current_task_index`.
            unsafe { (*current).save_mut().set_call(call) };
            // Safety: `current` is a valid, initialized entry of the task
            // table and no reference into the table is live.
            unsafe { crate::syscalls::syscall_entry(nr as u32, current) };
        }
        Err(fault) => {
            trace_task!(
                &[index],
                "task {:>width$}: process ended, {fault:?}",
                cfg.tasks[index].name
            );

            // Oh no, the interaction went poorly. Let's tell the kernel that
            // the current task is dead, and that it's time to switch to the
            // next reasonable task.

            with_task_table(|tasks| {
                let _ = task::force_fault(tasks, index, fault);
                let next = task::select(index, tasks);
                // Safety: `next` is in the task table.
                unsafe { next.switch_to() }
            });
        }
    }

    deliver_pending_irqs();
}

/// The idle task was selected: nothing can run until a timer fires. Advance
/// virtual time to the next deadline, or stop if there are no tasks waiting
/// on timers at all.
fn idle_step(idle_index: usize, width: usize) {
    if deliver_pending_irqs() {
        return;
    }
    with_task_table(|tasks| {
        let next_deadline = tasks.iter().filter_map(|t| t.timer().0).min();
        let Some(deadline) = next_deadline else {
            finish("every task is blocked and no timer is pending")
        };
        let target = deadline.max(Host::now());
        if let Some(stop) = config().stop_at
            && u64::from(target) > stop
        {
            finish(format_args!(
                "time reached the configured stop at {stop} ticks"
            ))
        }

        if config().realtime {
            // If we're in "realtime" mode, we want to appear to operate at
            // normal speed, so wait for the deadline to actually arrive.
            let wait = u64::from(target).saturating_sub(u64::from(Host::now()));
            std::thread::sleep(Duration::from_millis(wait));
        } else {
            // If we're *not* in "realtime" mode, oh look, it's time to resume!
            TICKS.store(target.into(), Ordering::Relaxed);
        }
        // Not attributable to a task: shown only when tracing everything.
        let name = "idle";
        trace_task!(&[], "task {name:>width$}: waited for the next deadline");
        crate::profiling::event_timer_isr_enter();
        let _ = task::process_timers(tasks, Host::now());
        crate::profiling::event_timer_isr_exit();
        let next = task::select(idle_index, tasks);
        // Safety: `next` is in the task table.
        unsafe { next.switch_to() }
    });
}

/// Posts any software-pended interrupts to their owners. Returns whether
/// that changed which task is current.
fn deliver_pending_irqs() -> bool {
    let irqs = std::mem::take(
        &mut *PENDING_IRQS.lock().unwrap_or_else(|e| e.into_inner()),
    );
    if irqs.is_empty() {
        return false;
    }
    let (_, index) = current_task_index();
    crate::profiling::event_isr_enter();
    let switched = with_task_table(|tasks| {
        let mut woke = false;
        for irq in irqs {
            let owner = crate::startup::HUBRIS_IRQ_TASK_LOOKUP.get(irq);
            let Some(owner) = owner else {
                continue;
            };
            // As on hardware, an interrupt is masked once delivered until the
            // task re-enables it.
            Host::disable_irq(irq.0, false).ok();
            woke |= tasks[owner.task as usize]
                .post(NotificationSet(owner.notification));
        }
        if woke {
            let next = task::select(index, tasks);
            // Safety: `next` is in the task table.
            unsafe { next.switch_to() }
        }
        woke
    });
    crate::profiling::event_isr_exit();
    switched
}

/////////////////////////////////////////////////////////////////////////////
// Implementation of `kern::arch::Arch`

/// Region descriptors carry no host-specific data.
#[derive(Copy, Clone, Debug)]
pub struct RegionDescExt;

// Used by the `build.rs`, this does nothing on hosted environments.
pub const fn compute_region_extension_data(
    _base: usize,
    _size: usize,
    _attributes: RegionAttributes,
) -> RegionDescExt {
    RegionDescExt
}

/// The host implementation of the kernel's architecture interface.
pub struct Host;

unsafe extern "C" fn default_handler() {
    panic!("unexpected interrupt on the host");
}

impl Arch for Host {
    type SavedState = SavedState;
    type RegionDescExt = RegionDescExt;

    /// There are no hardware interrupts on the host, so nothing is ever
    /// vectored here.
    const DEFAULT_HANDLER: unsafe extern "C" fn() = default_handler;

    /// Loads the run configuration, so a misconfigured run fails before any
    /// task starts, and starts the clock. Tick divisor unused.
    unsafe fn set_clock_freq(_tick_divisor: u32) {
        let _ = config();
        let _ = START.get_or_init(Instant::now);
    }

    fn reinitialize(task: &mut Task) {
        *task.save_mut() = SavedState::default();
        spawn_task(usize::from(task.descriptor().index));
    }

    /// Nothing to do: the kernel never dereferences task addresses, and task
    /// processes are isolated by the host already.
    fn apply_memory_protection(_task: &Task) {}

    fn start_first_task(_tick_divisor: u32, task: &mut Task) -> ! {
        // Safety: `task` is in the task table, per `start_kernel`.
        unsafe { Self::set_current_task(task) };
        loop {
            step();
        }
    }

    /// Records `task` as the one to resume next.
    ///
    /// # Safety
    ///
    /// `task` must be an entry of the task table, and the caller must not keep
    /// using the reference once the kernel returns to the scheduler loop.
    unsafe fn set_current_task(task: &mut Task) {
        let task: *mut Task = task;
        CURRENT_TASK_PTR.store(task, Ordering::Relaxed);
        crate::profiling::event_context_switch(task as usize);
    }

    fn now() -> Timestamp {
        if config().realtime {
            let start = START.get_or_init(Instant::now);
            Timestamp::from(start.elapsed().as_millis() as u64)
        } else {
            Timestamp::from(TICKS.load(Ordering::Relaxed))
        }
    }

    fn disable_irq(
        n: u32,
        _also_clear_pending: bool,
    ) -> Result<(), UsageError> {
        ENABLED_IRQS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&n);
        Ok(())
    }

    fn enable_irq(n: u32, _also_clear_pending: bool) -> Result<(), UsageError> {
        ENABLED_IRQS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(n);
        Ok(())
    }

    fn irq_status(n: u32) -> Result<IrqStatus, UsageError> {
        let mut status = IrqStatus::empty();
        let enabled = ENABLED_IRQS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&n);
        status.set(IrqStatus::ENABLED, enabled);
        let pending = PENDING_IRQS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&InterruptNum(n));
        status.set(IrqStatus::PENDING, pending);
        Ok(status)
    }

    fn pend_software_irq(irq: InterruptNum) -> Result<(), UsageError> {
        if crate::startup::HUBRIS_IRQ_TASK_LOOKUP.get(irq).is_none() {
            return Err(UsageError::NoIrq);
        }
        PENDING_IRQS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(irq);
        Ok(())
    }

    fn reset() -> ! {
        finish("the supervisor asked for a system reset")
    }
}

impl AtomicExt for AtomicBool {
    type Primitive = bool;

    #[inline(always)]
    fn swap_polyfill(&self, value: bool, ordering: Ordering) -> bool {
        self.swap(value, ordering)
    }
}
