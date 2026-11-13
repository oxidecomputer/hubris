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
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use abi::{InterruptNum, IrqStatus, Sysnum, TaskId, UsageError};

use crate::arch::Arch;
use crate::atomic::AtomicExt;
use crate::descs::{RegionAttributes, RegionDesc};
use crate::startup::with_task_table;
use crate::task::{self, ArchState, NotificationSet, Task};
use crate::time::Timestamp;

/// Ends the run normally: the simulated system has nothing left to do.
///
/// Also attempts to gracefully stop any tasks by informing them the kernel
/// is shutting down.
macro_rules! finish {
    ($($arg:tt)*) => {{
        eprintln!("kernel: stopping: {}", format_args!($($arg)*));
        host_task::shutdown_tasks();
        std::process::exit(0)
    }};
}

/// Same as `finish!`, but returns a non-zero exit code when a task has
/// called the `halt()` function.
macro_rules! halt {
    ($($arg:tt)*) => {{
        eprintln!("kernel: halting: {}", format_args!($($arg)*));
        host_task::shutdown_tasks();
        std::process::exit(hostcall::KERNEL_EXIT_HALTED)
    }};
}

/// robo: Ends the run as a failure with exit status `$code`, for `--timeout`.
macro_rules! fail {
    ($code:expr, $($arg:tt)*) => {{
        eprintln!("kernel: failing: {}", format_args!($($arg)*));
        host_task::shutdown_tasks();
        std::process::exit($code)
    }};
}

/// The kernel's tracing output
#[macro_use]
mod trace;

/// Configuration loaded from environment variables or files
mod config;

/// Interactions with task processes
mod host_task;

use config::{HOST_TASKS, MAX_TASK_NAME_WIDTH, options};
use host_task::{Call, PROCESSES, SavedState, spawn_task};
use trace::{describe_syscall, syscall_name, tracing};

/// Get the name of the task based on its index.
fn task_name_from_idx(index: usize) -> &'static str {
    match HOST_TASKS.get(index) {
        Some(task) => task.name,
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
static START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Software interrupts pended by `pend_software_irq`, delivered at the next
/// scheduling point.
static PENDING_IRQS: Mutex<Vec<InterruptNum>> = Mutex::new(Vec::new());

/// Interrupts a task has enabled, so `irq_status` can report them.
static ENABLED_IRQS: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

fn current_task_index() -> (*mut Task, usize) {
    let current = CURRENT_TASK_PTR.load(Ordering::Relaxed);
    if current.is_null() {
        panic!("no current task");
    }
    // Safety: the pointer came from a `&mut Task` in the task table (see
    // `set_current_task`), and this is only called between kernel entries,
    // when no other reference into the table is live.
    let index = usize::from(unsafe { &*current }.descriptor().index);
    (current, index)
}

/// One trip through the scheduler: resume the current task, wait for its
/// next syscall, and run the kernel for it.
fn step() {
    // Check if we're already done
    if let Some(stop) = options().stop_at
        && u64::from(Host::now()) > stop
    {
        finish!("time reached the configured stop at {stop} ticks")
    }
    if let Some(timeout) = options().timeout
        && u64::from(Host::now()) > timeout
    {
        fail!(
            hostcall::KERNEL_EXIT_TIMEOUT,
            "time reached the configured timeout at {timeout} ticks"
        )
    }

    let (current, index) = current_task_index();
    let is_idle = HOST_TASKS.get(index).is_some_and(|t| t.idle);

    if !is_idle {
        // Not idle, do a real task step.
        task_step(current, index);
    } else {
        let next_deadline = with_task_table(|tasks| {
            tasks.iter().filter_map(|t| t.timer().0).min()
        });
        // For now, we don't have external interrupts, which means that if we
        // have no deadlines and nothing to do, we'd be waiting forever. Just
        // bail instead.
        let Some(deadline) = next_deadline else {
            if options().timeout.is_some() {
                fail!(
                    hostcall::KERNEL_EXIT_DEADLOCK,
                    "every task is blocked and no timer is pending"
                );
            }
            finish!("every task is blocked and no timer is pending");
        };
        // Is idle, wait for the next deadline
        idle_wait(deadline);
    }

    // Re-read the current task index in case the currently selected task
    // changed while we were working on it (if it faulted, etc.). We can't
    // do this conditionally inside `with_task_table` because we are also
    // reading from a task table entry while getting the current index.
    let (_, index) = current_task_index();

    // Process timers
    let now = Host::now();
    crate::profiling::event_timer_isr_enter();
    with_task_table(|tasks| {
        if task::process_timers(tasks, now) != task::NextTask::Same {
            let next = task::select(index, tasks);
            // Safety: `next` is in the task table.
            unsafe { next.switch_to() }
        }
    });
    crate::profiling::event_timer_isr_exit();

    // Ignore whether delivering IRQs changed the "next" task, since we're going
    // to go back around to the scheduler anyway.
    _ = deliver_pending_irqs();
}

/// Process a single yield-to-task step.
fn task_step(current: *mut Task, index: usize) {
    let width = MAX_TASK_NAME_WIDTH;

    // If this isn't an idle task, and the kernel has decided to schedule this
    // task, then we either have something to tell it, or the task has just
    // been (re-)spawned.

    // Interact with the external task process
    let res = {
        // Get the process handle we are about to interact with
        let mut procs = PROCESSES.lock().unwrap();
        let Some(proc) = procs.get_mut(index).and_then(Option::as_mut) else {
            panic!("task {index} has no process to resume")
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
                HOST_TASKS[index].name,
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
                    HOST_TASKS[index].name
                );
            }

            // The interaction was successful! We gave the task our [`Return`],
            // and it gave us a new [`Syscall`] (which we've converted into a
            // [`Call`]). Let's store that off, and then tell the kernel that
            // there is a new syscall to handle.

            let nr = call.syscall_nr;
            // Safety: as in `current_task_index`.
            unsafe { (*current).save_mut().set_call(call) };

            // Detect the `halt()` userlib call, which is a closed receive to
            // the kernel with an empty notification mask. We call tasks "dead"
            // to match what `humility` reports them as in `tasks`.
            if nr == Sysnum::Recv {
                // Safety: `current` is a valid, initialized entry of the task
                // table and no reference into the table is live.
                let args = unsafe { (*current).save().as_recv_args() };
                if args.notification_mask == 0
                    && args.specific_sender == Some(TaskId::KERNEL)
                {
                    let name = HOST_TASKS[index].name;
                    if options().ignore_halted_tasks {
                        event!(
                            "task {name:>width$}: dead, waiting on the kernel \
                             with an empty notification mask"
                        );
                    } else {
                        halt!(
                            "task {name} is dead: it is waiting on the kernel \
                             with an empty notification mask. Pass \
                             `--ignore-halted-tasks` to ignore task calls to \
                             `halt()`."
                        );
                    }
                }
            }
            // Safety: `current` is a valid, initialized entry of the task
            // table and no reference into the table is live.
            unsafe { crate::syscalls::syscall_entry(nr as u32, current) };
        }
        Err(fault) => {
            event!(
                "task {:>width$}: process ended, {fault:?}",
                HOST_TASKS[index].name
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
}

/// The idle task was selected: nothing can run until a timer fires. Advance
/// virtual time to the next deadline.
fn idle_wait(deadline: Timestamp) {
    let width = MAX_TASK_NAME_WIDTH;
    let mut target = deadline.max(Host::now());
    // Clamp the time we wait until to the stop time, if set. This means we
    // can "kill" the simulation while sitting in idle instead of when the next
    // task would resume.
    if let Some(stop) = options().stop_at.or(options().timeout) {
        target = target.min(Timestamp::from(stop + 1));
    }

    if options().realtime {
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
}

/// Posts any software-pended interrupts to their owners. Returns whether
/// that changed which task is current.
#[must_use]
fn deliver_pending_irqs() -> bool {
    let irqs: Vec<_> = {
        let mut guard = PENDING_IRQS.lock().unwrap();
        std::mem::take(&mut guard)
    };
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

// Used by the `build.rs`, this is a trival wrapper on hosted environments.
pub const fn compute_region_desc(
    base: usize,
    size: usize,
    attributes: RegionAttributes,
) -> RegionDesc {
    RegionDesc {
        base,
        size,
        attributes,
        arch_data: RegionDescExt,
    }
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
        let _ = options();
        LazyLock::force(&START);
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
        if options().realtime {
            Timestamp::from(START.elapsed().as_millis() as u64)
        } else {
            Timestamp::from(TICKS.load(Ordering::Relaxed))
        }
    }

    fn disable_irq(
        n: u32,
        _also_clear_pending: bool,
    ) -> Result<(), UsageError> {
        ENABLED_IRQS.lock().unwrap().remove(&n);
        Ok(())
    }

    fn enable_irq(n: u32, _also_clear_pending: bool) -> Result<(), UsageError> {
        ENABLED_IRQS.lock().unwrap().insert(n);
        Ok(())
    }

    fn irq_status(n: u32) -> Result<IrqStatus, UsageError> {
        let mut status = IrqStatus::empty();
        let enabled = ENABLED_IRQS.lock().unwrap().contains(&n);
        status.set(IrqStatus::ENABLED, enabled);
        let pending = PENDING_IRQS.lock().unwrap().contains(&InterruptNum(n));
        status.set(IrqStatus::PENDING, pending);
        Ok(status)
    }

    fn pend_software_irq(irq: InterruptNum) -> Result<(), UsageError> {
        if crate::startup::HUBRIS_IRQ_TASK_LOOKUP.get(irq).is_none() {
            return Err(UsageError::NoIrq);
        }
        PENDING_IRQS.lock().unwrap().push(irq);
        Ok(())
    }

    fn reset() -> ! {
        finish!("the supervisor asked for a system reset")
    }
}

impl AtomicExt for AtomicBool {
    type Primitive = bool;

    #[inline(always)]
    fn swap_polyfill(&self, value: bool, ordering: Ordering) -> bool {
        self.swap(value, ordering)
    }
}
