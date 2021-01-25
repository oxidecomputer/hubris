#![no_std]

use drv_lpc55_syscon_api::{Peripheral, Syscon};
use userlib::*;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = Task::anonymous;

pub fn casper_init() {
    let syscon =
        TaskId::for_index_and_gen(SYSCON as usize, Generation::default());
    let syscon = Syscon::from(syscon);

    syscon.enable_clock(Peripheral::Casper);
    syscon.leave_reset(Peripheral::Casper);
}

pub fn caspar_add64(a: u32, b: u32, c: &mut u32, d: &mut u32) {
    let casper = unsafe { &*lpc55_pac::CASPER::ptr() };

    const AB_LOCATION: u32 = 0x200 as u32;

    const RES_LOCATION: u32 = 0x400 as u32;

    const SRAM: u32 = 0x1400_0000 as u32;

    // RES0 = RES0 + B
    // RES1 = RES1 + A
    unsafe {
        // B
        core::ptr::write_volatile((SRAM + AB_LOCATION) as *mut u32, b);
        // A
        core::ptr::write_volatile((SRAM + 0x4000 + AB_LOCATION) as *mut u32, a);
        // RES0
        core::ptr::write_volatile((SRAM + RES_LOCATION) as *mut u32, b);
        // RES1
        core::ptr::write_volatile(
            (SRAM + 0x4000 + RES_LOCATION) as *mut u32,
            a,
        );

        // The SVD is wrong in a couple of ways:
        // 1) The ABOFF field CTRL0 is marked as a single bit
        // 2) The entries for ABOFF and RESOFF are actually written to bit
        // offset 0 and 16 respectively with the lower bits masked and used for
        // other information, similar to the MPU/SAU work.
        // Just write things manually for now.
        core::ptr::write_volatile(
            0x400a_5000 as *mut u32,
            (AB_LOCATION << 1) as u32,
        );

        // command 0x8 = ADD64
        core::ptr::write_volatile(
            0x400a_5004 as *mut u32,
            (((RES_LOCATION << 1) << 16) | 0x8 << 8) as u32,
        );
    }

    while !casper.status.read().done().is_completed() {}

    *c = unsafe {
        core::ptr::read_volatile((SRAM + RES_LOCATION) as *const u32)
    };
    *d = unsafe {
        core::ptr::read_volatile((SRAM + RES_LOCATION + 0x4000) as *const u32)
    };
}
