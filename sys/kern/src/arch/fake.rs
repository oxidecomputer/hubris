// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::sync::atomic::{AtomicU32, Ordering};

macro_rules! uassert {
    ($cond:expr) => {
        assert!($cond)
    };
}

#[derive(Default, Debug)]
pub struct SavedState {}

impl crate::task::ArchState for SavedState {
    /// TODO: this is probably not needed here.
    fn stack_pointer(&self) -> u32 {
        0
    }

    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32 {
        0
    }
    /// Reads syscall argument register 1.
    fn arg1(&self) -> u32 {
        0
    }
    /// Reads syscall argument register 2.
    fn arg2(&self) -> u32 {
        0
    }
    /// Reads syscall argument register 3.
    fn arg3(&self) -> u32 {
        0
    }
    /// Reads syscall argument register 4.
    fn arg4(&self) -> u32 {
        0
    }
    /// Reads syscall argument register 5.
    fn arg5(&self) -> u32 {
        0
    }
    /// Reads syscall argument register 6.
    fn arg6(&self) -> u32 {
        0
    }

    /// Reads the syscall descriptor (number).
    fn syscall_descriptor(&self) -> u32 {
        0
    }

    /// Writes syscall return argument 0.
    fn ret0(&mut self, _: u32) {}
    /// Writes syscall return argument 1.
    fn ret1(&mut self, _: u32) {}
    /// Writes syscall return argument 2.
    fn ret2(&mut self, _: u32) {}
    /// Writes syscall return argument 3.
    fn ret3(&mut self, _: u32) {}
    /// Writes syscall return argument 4.
    fn ret4(&mut self, _: u32) {}
    /// Writes syscall return argument 5.
    fn ret5(&mut self, _: u32) {}
}

pub fn reset() -> ! {
    panic!("SYSTEM RESET");
}

static CLOCK_FREQ: AtomicU32 = AtomicU32::new(0);

pub unsafe fn set_clock_freq(f: u32) {
    CLOCK_FREQ.store(f, Ordering::Relaxed);
}

pub fn reinitialize(task: &mut crate::task::Task) {
    println!("reinitialize");
}

pub fn disable_irq(irq: u32) {}

pub fn enable_irq(irq: u32) {}

pub fn apply_memory_protection(task: &crate::task::Task) {}

pub unsafe fn set_current_task(task: &crate::task::Task) {}

pub fn start_first_task(tick_divisor: u32, task: &mut crate::task::Task) -> ! {
    panic!("entering userland");
}

pub fn now() -> crate::time::Timestamp {
    0.into()
}

impl crate::atomic::AtomicExt for core::sync::atomic::AtomicBool {
    type Primitive = bool;
    fn swap_polyfill(
        &self,
        value: Self::Primitive,
        ordering: Ordering,
    ) -> Self::Primitive {
        self.swap(value, ordering)
    }
}
