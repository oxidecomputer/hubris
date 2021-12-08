// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A driver for the STM32H7 ECC management

#![no_std]
#![no_main]

use userlib::*;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[export_name = "main"]
fn main() -> ! {
    init();

    sys_irq_control(1, true);

    loop {
        let _ = sys_recv_closed(&mut [], 1, TaskId::KERNEL);

        // There are three types of errors to handle. They fit into two
        // categories - single and double error.

        // If the error is in an address range we don't care about, we can
        // ignore it (i.e. if the kernel or a task isn't mapped to that region)

        // Single error:
        // SEDCF: ECC single error detected and corrected flag
        // We can recover by re-writing the same data back to that address.
        // This will be a kernel concern as we'd need access to the full
        // address space to read an address and re-write to it

        // DEBWDF: ECC double error on byte write (BW) detected flag
        // DEDF: ECC double error detected flag
        // We cannot recover by re-writing the same data back to that address.
        // We need to get it from a source of truth or declare it lost.

        // If it's in a task .data region - we can rewrite that memory with the
        // correct data from flash, and restart the task as a precaution.
        // If it's in a task stack region - we can restart the task, after
        // which we don't care what we lost from that address.

        // If it's in kernel .data region - we could rewrite that memory with
        // the correct data from flash, though might choose to reboot the
        // machine instead.
        // If it's in the kernel stack region - rebooting the machine may be
        // the only safe option.
    }
}

// Enable the interrupts for error correction and detection
// Currently only using DTCM so using ECC monitors 3 and 4
fn init() {
    // Clear the RAM ECC status register flags for monitors 3 and 4
    let ramecc1 = unsafe { &*device::RAMECC1::ptr() };
    ramecc1.m3sr.modify(|_, w| { w.debwdf().clear_bit().dedf().clear_bit().sedcf().clear_bit() });
    ramecc1.m4sr.modify(|_, w| { w.debwdf().clear_bit().dedf().clear_bit().sedcf().clear_bit() });

    // Next activate ECC error latching and interrupts for monitors 3 and 4.
    // ECCELEN: ECC error latching enable - capture context for ECC error generated
    // ECCDEBWIE: ECC double error on byte write (BW) interrupt enable
    // ECCDEIE: ECC double error interrupt enable
    // ECCSEIE: ECC single error interrupt enable
    ramecc1.m3cr.modify(|_, w| { w.eccelen().bit(true).eccdebwie().bit(true).eccdeie().bit(true).eccseie().bit(true) });
    ramecc1.m4cr.modify(|_, w| { w.eccelen().bit(true).eccdebwie().bit(true).eccdeie().bit(true).eccseie().bit(true) });

    // Finally enable the global RAM ECC interrupts, which should only fire for
    // monitors 3 and 4 currently
    ramecc1.ier.modify(|_, w| { w.geccdebwie().bit(true).geccdeie().bit(true).geccseie().bit(true).gie().bit(true) });
}
