// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_main]
#![no_std]

use cortex_m::peripheral::mpu::RegisterBlock;

/// Disable the MPU and use the default memory map
///
/// The default memory map applies to accesses from both privileged and
/// unprivileged software.
///
/// This is the same behavior as when the MPU is not implemented.
pub unsafe fn disable_mpu(mpu: &RegisterBlock) {
    const DISABLE: u32 = 0b000;

    // From the ARMv8m MPU manual
    //
    // Any outstanding memory transactions must be forced to complete by
    // executing a DMB instruction and the MPU disabled before it can be
    // configured
    cortex_m::asm::dmb();
    mpu.ctrl.write(DISABLE);
}

/// Enable the MPU and set the default memory map as background region for privileged
/// software access if `privileged_default_memmap_access` is set to true.
///
/// If no regions are configured for the MPU, and
/// `privileged_default_memmap_access == true`, then only privileged software
/// may run.
///
/// If `privileged_default_memmap_access == false` then any memory access to a
/// location not covered by a configure region will cause a fault, regardless
/// of whether that access is made by by privileged software or not.
pub unsafe fn enable_mpu(
    mpu: &RegisterBlock,
    privileged_default_memmap_access: bool,
) {
    const ENABLE: u32 = 0b001;
    let privdefena: u32 = if privileged_default_memmap_access {
        0b100
    } else {
        0b000
    };

    mpu.ctrl.write(ENABLE | privdefena);
    // From the ARMv8m MPU manual
    //
    // The final step is to enable the MPU by writing to MPU_CTRL. Code
    // should then execute a memory barrier to ensure that the register
    // updates are seen by any subsequent memory accesses. An Instruction
    // Synchronization Barrier (ISB) ensures the updated configuration
    // [is] used by any subsequent instructions.
    cortex_m::asm::dmb();
    cortex_m::asm::isb();
}
