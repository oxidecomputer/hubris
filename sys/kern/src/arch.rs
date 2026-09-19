// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Architecture-specific support.
//!
//! In practice, this works by
//!
//! - Conditionally defining a nested module (below).
//! - `pub use`-ing its contents
//! - Naming that module's implementation of the [`Arch`] trait as
//!   [`Current`].
//!
//! Thus, all architecture-specific types and functions show up right here in
//! the `arch` module, magically tailored for the current target.
//!
//! The operations every architecture must provide are defined by the [`Arch`]
//! trait. The remaining names each architecture support module must define
//! are:
//!
//! - `SavedState`, implementing [`crate::task::ArchState`].
//! - `RegionDescExt` and `compute_region_extension_data`, which build the
//!   architecture's precomputed memory protection data. These are free items
//!   rather than trait members because the region table is built in `const`
//!   context, and trait methods can't be `const fn`.

use abi::{InterruptNum, IrqStatus, UsageError};

use crate::task::Task;
use crate::time::Timestamp;

cfg_if::cfg_if! {
    // Note: cfg_if! is slightly touchy about ordering and expression
    // complexity; this chain seems to be the best compromise.

    if #[cfg(not(target_os = "none"))] {
        // Not a Hubris target at all: the kernel is being built to run as a
        // host process for testing, with tasks as child processes.
        pub mod host;
        pub use host::*;

        /// The architecture the kernel is being built for.
        pub type Current = host::Host;
    } else if #[cfg(not(target_pointer_width = "32"))] {
        compile_error!("non-32-bit targets not supported");
    } else if #[cfg(target_arch = "arm")] {
        #[macro_use]
        pub mod arm_m;
        pub use arm_m::*;

        /// The architecture the kernel is being built for.
        pub type Current = arm_m::ArmM;
    } else {
        compile_error!("support for this architecture not implemented");
    }
}

/// Operations the architecture-independent kernel needs from the
/// architecture it runs on.
///
/// Implemented by a zero-sized type in each architecture support module, and
/// reached through [`Current`].
pub trait Arch {
    /// Records the kernel tick divisor, which is the clock frequency in kHz,
    /// before anything else in the kernel runs.
    ///
    /// # Safety
    ///
    /// Call this once, at the start of kernel startup.
    unsafe fn set_clock_freq(tick_divisor: u32);

    /// Resets `task`'s saved state to how it looked before the task first
    /// ran, so it restarts from its entry point.
    fn reinitialize(task: &mut Task);

    /// Applies `task`'s memory protection configuration, in preparation for
    /// running it.
    fn apply_memory_protection(task: &Task);

    /// Starts the kernel's scheduling of tasks, running `task` first.
    fn start_first_task(tick_divisor: u32, task: &mut Task) -> !;

    /// Records the address of `task` as the current user task.
    ///
    /// # Safety
    ///
    /// This records a pointer that aliases `task`. As long as you don't read
    /// that pointer while you have access to `task`, and as long as the
    /// `task` being stored is actually in the task table, you'll be okay.
    unsafe fn set_current_task(task: &mut Task);

    /// Reads the tick counter.
    fn now() -> Timestamp;

    /// Disables interrupt `n`, and clears any pending instance of it if
    /// `also_clear_pending` is set.
    fn disable_irq(n: u32, also_clear_pending: bool) -> Result<(), UsageError>;

    /// Enables interrupt `n`, first clearing any pending instance of it if
    /// `also_clear_pending` is set.
    fn enable_irq(n: u32, also_clear_pending: bool) -> Result<(), UsageError>;

    /// Returns a cross-platform representation of interrupt `n`'s status.
    fn irq_status(n: u32) -> Result<IrqStatus, UsageError>;

    /// Marks `irq` pending, as if the hardware had raised it.
    fn pend_software_irq(irq: InterruptNum) -> Result<(), UsageError>;

    /// Resets the system.
    fn reset() -> !;
}
