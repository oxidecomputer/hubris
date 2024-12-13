// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel online profiling support.
//!
//! This is intended to help measure the timing of kernel events, duration of
//! syscalls, and the like, as an aid to debugging or optimization work.
//!
//! Because the kernel is SoC-independent, this module does not assume any
//! particular way of getting profiling information out of the kernel. Instead,
//! any target that wants to use profiling needs to populate an `EventsTable`
//! struct and provide it to `kern::profiling::configure_events_table` from its
//! startup routine. This requires the target to provide functions for a set of
//! _events._ The currently defined events are listed in the docs for
//! `EventsTable`.
//!
//! A typical implementation would implement these event handling functions by
//! setting or clearing GPIOs on the processor package, where they can be
//! monitored and examined by an external logic analyzer. Other implementations
//! are of course possible, but be careful of probe effect and keep the handler
//! functions fast.

use core::sync::atomic::{AtomicPtr, Ordering};

/// Hooks that must be provided by the board setup code if it wants to enable
/// kernel profiling.
///
/// If you provide an `EventsTable`, you have to provide every hook. This
/// eliminates one null pointer check / conditional branch from each
/// _implemented_ profiling event, because we expect the implemented events to
/// outnumber the stubbed ones. If you would like to omit (say) the `isr_enter`
/// event, the simplest method is:
///
/// ```ignore
///   isr_enter: || (),
/// ```
pub struct EventsTable {
    /// Called on entry to the kernel syscall handler, in response to a task
    /// making a syscall.
    pub syscall_enter: fn(u32),
    /// Called on exit from the kernel syscall handler back to a task.
    pub syscall_exit: fn(),
    /// Called on entry to the kernel's "secondary" syscall entry point, which
    /// is typically used for context switching but is platform-specific.
    pub secondary_syscall_enter: fn(),
    /// Called on exit from the kernel's "secondary" syscall handler.
    pub secondary_syscall_exit: fn(),
    /// Called on entry to any kernel-managed peripheral interrupt service
    /// routine.
    pub isr_enter: fn(),
    /// Called on exit from any kernel-managed peripheral interrupt service
    /// routine.
    pub isr_exit: fn(),
    /// Called on entry to the kernel's timer ISR.
    pub timer_isr_enter: fn(),
    /// Called on exit from the kernel's timer ISR.
    pub timer_isr_exit: fn(),

    /// Called whenever the current task changes, with the index of the task.
    pub context_switch: fn(usize),
}

/// Supplies the kernel with an events table.
///
/// You can call this more than once if you need to, though that seems odd at
/// first glance.
pub fn configure_events_table(table: &'static EventsTable) {
    EVENTS_TABLE.store(table as *const _ as *mut _, Ordering::Relaxed);
}

/// Internal pointer written by `configure_events_table` and read by `table`. If
/// this is null, no event table has been provided.
///
/// Any non-null pointed-to table is guaranteed (by the other code in this
/// module) to have static scope.
///
/// Note: all accesses to this atomic value use `Relaxed` ordering, because we
/// expect it to get written once at startup and then read many times, and
/// memory barriers have non-zero cost.
static EVENTS_TABLE: AtomicPtr<EventsTable> =
    AtomicPtr::new(core::ptr::null_mut());

/// Grabs a reference to the configured table, if any.
fn table() -> Option<&'static EventsTable> {
    let p = EVENTS_TABLE.load(Ordering::Relaxed);
    if p.is_null() {
        None
    } else {
        // We only write this pointer from a valid `&'static`, and we're handing
        // out a shared reference, so this should be ok...
        unsafe { Some(&*p) }
    }
}

pub(crate) fn event_syscall_enter(nr: u32) {
    if let Some(t) = table() {
        (t.syscall_enter)(nr)
    }
}

pub(crate) fn event_syscall_exit() {
    if let Some(t) = table() {
        (t.syscall_exit)()
    }
}

pub(crate) fn event_secondary_syscall_enter() {
    if let Some(t) = table() {
        (t.secondary_syscall_enter)()
    }
}

pub(crate) fn event_secondary_syscall_exit() {
    if let Some(t) = table() {
        (t.secondary_syscall_exit)()
    }
}

/// Signals entry to an ISR. This is `pub` in case you write your own
/// non-kernel-managed ISR but you'd like to include it in ISR statistics.
pub fn event_isr_enter() {
    if let Some(t) = table() {
        (t.isr_enter)()
    }
}

/// Signals exit from an ISR. This is `pub` in case you write your own
/// non-kernel-managed ISR but you'd like to include it in ISR statistics.
pub fn event_isr_exit() {
    if let Some(t) = table() {
        (t.isr_exit)()
    }
}

pub(crate) fn event_timer_isr_enter() {
    if let Some(t) = table() {
        (t.timer_isr_enter)()
    }
}

pub(crate) fn event_timer_isr_exit() {
    if let Some(t) = table() {
        (t.timer_isr_exit)()
    }
}

pub(crate) fn event_context_switch(idx: usize) {
    if let Some(t) = table() {
        (t.context_switch)(idx)
    }
}
