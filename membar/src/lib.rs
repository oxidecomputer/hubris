//! Mostly-portable memory barrier operations.
//!
//! These barrier operations are intended to help you reason about the ordering
//! of memory operations as issued from the processor.
//!
//! If you're trying to reason about the ordering of _atomic_ memory accesses,
//! you probably want `core::sync::atomic::fence` instead. This crate is mostly
//! intended for ordering `volatile` accesses, which have no defined interaction
//! with atomics, for better or worse.
//!
//! Note that there are two things this crate explicitly does _not_ do:
//!
//! 1. **Cache maintenance.** If you're trying to get data written out to main
//!    memory so that it becomes visible to a DMA-capable peripheral, for
//!    example, a memory barrier is likely _necessary_ but is almost certainly
//!    not _sufficient._ Knowing what _is_ sufficient requires system-specific
//!    knowledge and is out of scope here.
//!
//! 2. **Self-modifying code.** The barriers provided by this crate are not
//!    sufficient to handle cases where you have made changes to the code you're
//!    executing -- or certain other actions that look similar, such as
//!    remapping the memory you're executing from, altering its execute or
//!    no-execute bits, etc. Again, doing this correctly requires more
//!    system-specific knowledge than this crate can reasonably possess.

#![no_std]
#![feature(asm)]

cfg_if::cfg_if! {
    if #[cfg(any(target_arch = "arm"))] {
        mod arm;
        use arm::*;
    } else if #[cfg(target_arch = "x86_64")] {
        mod x86_64;
        use x86_64::*;
    } else {
        compile_error!("memory barriers are not defined for this target yet");
    }
}

/// Ensure that the data for any loads _before_ the barrier is accessed before
/// any loads _after_ the barrier are performed.
#[inline(always)]
pub fn load_load_barrier() {
    arch_specific_load_load();
}

/// Ensure that the data for any loads _before_ the barrier is accessed before
/// any store _after_ the barrier are performed.
#[inline(always)]
pub fn load_store_barrier() {
    arch_specific_load_store();
}

/// Ensure that the data written by any stores _before_ the barrier is made
/// visible before the data written by any store _after_ the barrier.
#[inline(always)]
pub fn store_store_barrier() {
    arch_specific_store_store();
}

/// Ensure that the data written by any stores _before_ the barrier is made
/// visible before the data for any load _after_ the barrier is accessed.
///
/// This ensures that loads after the barrier are not simply served from a
/// "store buffer" bypassing the memory subsystem.
#[inline(always)]
pub fn store_load_barrier() {
    arch_specific_store_load();
}
