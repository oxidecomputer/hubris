// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Application configuration passed into the kernel build.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelConfig {
    /// Features enabled in the kernel
    pub features: Vec<String>,

    /// External regions used in the kernel
    pub extern_regions: BTreeMap<String, std::ops::Range<u32>>,

    /// Tasks in the app image. The order of tasks is significant.
    pub tasks: Vec<TaskConfig>,

    /// Regions that tasks have shared access to, keyed by the name the task
    /// config used to grant access (often peripheral name). These are typically
    /// memory mapped peripherals.
    pub shared_regions: BTreeMap<String, RegionConfig>,

    /// Interrupts hooked by the application, keyed by IRQ number.
    pub irqs: BTreeMap<u32, InterruptConfig>,
}

/// Configuration for a single hooked interrupt.
#[derive(
    Copy,
    Clone,
    Debug,
    Serialize,
    Deserialize,
    Eq,
    PartialEq,
    Hash,
    Ord,
    PartialOrd,
)]
pub struct InterruptConfig {
    /// Index of task (in the application task array) that receives this
    /// interrupt.
    pub task_index: usize,
    /// Notification bits that are posted to the task when the interrupt fires.
    /// Note that this is a mask and can have multiple (or zero!) bits set; the
    /// kernel doesn't really care.
    pub notification: u32,
}

/// Record describing a single task.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskConfig {
    /// Named memory regions that this task has exclusive access to, keyed by
    /// name.
    ///
    /// The name is the "output" assignment that generated this region,
    /// typically (but not necessarily!) either `"ram"` or `"flash"`.
    pub owned_regions: BTreeMap<String, MultiRegionConfig>,

    /// Names of regions (in the app-level `shared_regions`) that this task
    /// needs access to.
    pub shared_regions: BTreeSet<String>,

    /// Address of the task's entry point. This is the first instruction that
    /// will be executed whenever the task is (re)started.
    pub entry_point: OwnedAddress,

    /// Address of the task's initial stack pointer, to be loaded at (re)start.
    /// It must be pointing into or *just past* one of the task's memory
    /// regions.
    pub initial_stack: OwnedAddress,

    /// Initial priority of this task.
    pub priority: u8,

    /// Should this task be started automatically on boot?
    pub start_at_boot: bool,
}

/// An address within an owned region of memory.
///
/// Certain analyses benefit from being able to tell that an address like a
/// stack pointer points into a particular class of memory region. While we
/// could determine this by e.g. comparing the address to all memory regions,
/// this type explicitly encodes the intended relationship between an address
/// and region, simplifying the analysis.
///
/// Note that an `OwnedAddress` can encode an offset that is out of range for
/// the region. This is an error and should be rejected. As a special case,
/// certain applications (particularly stack pointers) accept an "off the end"
/// address in a region, since the address will not be directly dereferenced.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OwnedAddress {
    /// Name of region in the task's `owned_regions` table.
    pub region_name: String,
    /// Offset within the region.
    pub offset: u32,
}

/// Description of one memory region.
///
/// A memory region spans a range of physical addresses, and applies access
/// permissions to whatever lies in that range. Despite our use of the term
/// "memory" here, the region may not describe RAM -- ROM and memory-mapped
/// peripherals are also described by memory regions.
///
/// A memory region can be used by multiple tasks. This is mostly used to have
/// tasks share a no-access region (often index 0) in unused region slots, but
/// you could also use it for shared peripheral or RAM access.
#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct RegionConfig {
    /// Address of start of region. The platform likely has alignment
    /// requirements for this; it must meet them. (For example, on ARMv7-M, it
    /// must be naturally aligned for the size.)
    pub base: u32,
    /// Size of region, in bytes. The platform likely has alignment requirements
    /// for this; it must meet them. (For example, on ARMv7-M, it must be a
    /// power of two greater than 16.)
    pub size: u32,
    /// Flags describing what can be done with this region.
    pub attributes: RegionAttributes,
}

/// Description of one memory span containing multiple adjacent regions
///
/// This is equivalent to [`RegionConfig`], but represents a single memory span
/// that should be configured as multiple regions in the MPU.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MultiRegionConfig {
    /// Address of start of region. The platform likely has alignment
    /// requirements for this; it must meet them. (For example, on ARMv7-M, it
    /// must be naturally aligned for the size.)
    pub base: u32,
    /// Size of region, in bytes for each chunk. The platform likely has
    /// alignment requirements for this; it must meet them. (For example, on
    /// ARMv7-M, it must be a power of two greater than 16.)
    pub sizes: Vec<u32>,
    /// Flags describing what can be done with this region.
    pub attributes: RegionAttributes,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct RegionAttributes {
    /// Region can be read by tasks that include it.
    pub read: bool,
    /// Region can be written by tasks that include it.
    pub write: bool,
    /// Region can contain executable code for tasks that include it.
    pub execute: bool,
    /// Special role assigned to this region, if any. This controls cache
    /// behavior, among other things.
    pub special_role: Option<SpecialRole>,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub enum SpecialRole {
    /// Region contains memory mapped registers. This affects cache behavior
    /// on devices that include it, and discourages the kernel from using
    /// `memcpy` in the region.
    Device,
    /// Region can be used for DMA or communication with other processors.
    /// This heavily restricts how this memory can be cached and will hurt
    /// performance if overused.
    Dma,
}
