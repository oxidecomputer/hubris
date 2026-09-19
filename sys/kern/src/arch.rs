// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Architecture-specific support.
//!
//! The operations every architecture must provide are defined by the [`Arch`]
//! trait. In practice, this works by each architecture
//!
//! - Implementing the [`Arch`] trait on a ZST struct type.
//! - Conditionally defining a nested module (below).
//! - `pub use`-ing the [`Arch`] trait impl under the name `Current` within
//!   the conditional code below.
//!
//! The rest of the kernel (ideally) ONLY uses the `arch::Current` type and
//! its associated types and methods to perform behavior, allowing for
//! arch-independent operation.

use abi::{InterruptNum, IrqStatus, UsageError};

use crate::task::{ArchState, Task};
use crate::time::Timestamp;

cfg_if::cfg_if! {
    // Note: cfg_if! is slightly touchy about ordering and expression
    // complexity; this chain seems to be the best compromise.

    if #[cfg(not(target_pointer_width = "32"))] {
        compile_error!("non-32-bit targets not supported (even for simulation)");
    } else if #[cfg(target_arch = "arm")] {
        pub mod arm_m;

        /// The architecture the kernel is being built for.
        pub use arm_m::ArmM as Current;
    } else {
        compile_error!("support for this architecture not implemented");
    }
}

/// Operations the architecture-independent kernel needs from the
/// architecture it runs on.
///
/// See [crate::arch] for how this is intended to work
pub trait Arch {
    /// Architecture specific saved state for each Task
    type SavedState: ArchState;

    /// Architecture-specific data for memory regions, which can be used when
    /// operating on tasks.
    ///
    /// Each [`Task`] contains a `&[RegionData]`, and `RegionData` contains a
    /// `RegionDescExt`.
    ///
    /// For example: used for storing pre-computed MPU information for Cortex-M
    /// processors to speed task switching.
    type RegionDescExt;

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
