// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

#[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
mod dice;
#[cfg(feature = "dice-mfg")]
mod dice_mfg_usart;
mod images;

pub mod handoff;
use handoff::Handoff;

use armv8_m_mpu::{disable_mpu, enable_mpu};
use cortex_m::peripheral::MPU;
use stage0_handoff::{RotBootState, RotSlot};

const ROM_VER: u32 = 1;

// Setup the MPU so that we can treat the USB RAM as normal RAM, and not as a
// peripheral. Specifically we want to clear the `DEVICE` attributes, so that
// we can allow unaligned access.
//
// If this is called from the same execution mode as the Hubris kernel, note
// that these settings will be replaced when the kernel starts -- we only need
// them to apply for now while we write that memory.
fn apply_memory_protection(mpu: &MPU) {
    unsafe {
        disable_mpu(&mpu);
    }

    const USB_RAM_BASE: u32 = 0x4010_0000;
    const USB_RAM_SIZE: u32 = 0x4000;
    const USB_RAM_REGION_NUMBER: u32 = 0;

    // Subtract 32 because the `LIMIT` field in the `rlar` register range is inclusive
    // and then enable the region (bit 0).
    let rlar = (USB_RAM_BASE + USB_RAM_SIZE - 32) | (1 << 0);

    let ap = 0b01; // read-write by any privilege level
    let sh = 0b00; // non-shareable - we only use one core with no DMA here
    let xn = 1; // Don't execute out of this region
    let rbar = USB_RAM_BASE | (sh as u32) << 3 | ap << 1 | xn;

    // region 0: write-back transient, not shared, with read/write supported
    let mair0 = 0b0111_0100;

    unsafe {
        mpu.rnr.write(USB_RAM_REGION_NUMBER);
        // We only have one region (0), so no need for a RMW cycle
        mpu.mair[0].write(mair0);
        mpu.rbar.write(rbar);
        mpu.rlar.write(rlar);
    }

    unsafe {
        enable_mpu(&mpu, true);
    }
}

pub fn startup(
    core_peripherals: &cortex_m::Peripherals,
    peripherals: &lpc55_pac::Peripherals,
) {
    // This is the SYSCON_DIEID register on LPC55 which contains the ROM
    // version. Make sure our configuration matches!
    let val = unsafe { core::ptr::read_volatile(0x50000ffc as *const u32) };

    if val & 1 != ROM_VER {
        panic!()
    }

    let mpu = &core_peripherals.MPU;

    // Turn on the memory used by the handoff subsystem to dump
    // `RotUpdateDetails` and DICE information required by hubris.
    //
    // This allows hubris tasks to always get valid memory, even if it is all
    // 0's.
    let handoff = Handoff::turn_on(&peripherals.SYSCON);

    apply_memory_protection(mpu);

    #[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
    dice::run(&handoff, &peripherals);

    // Write the image details to handoff RAM. Use the address of the current
    // function to determine which image is running.
    let img_a = images::get_image_a();
    let img_b = images::get_image_b();
    let here = startup as *const u8;
    let active = if img_a.as_ref().map(|i| i.contains(here)).unwrap_or(false) {
        RotSlot::A
    } else if img_b.as_ref().map(|i| i.contains(here)).unwrap_or(false) {
        RotSlot::B
    } else {
        panic!();
    };
    let a = img_a.map(images::image_details);
    let b = img_b.map(images::image_details);

    let details = RotBootState { active, a, b };

    handoff.store(&details);
}

// When we're secure we don't have access to read the CMPA/NMPA where the
// official setting is stored, emulate what the clock driver does instead
pub fn get_clock_speed(peripherals: &lpc55_pac::Peripherals) -> (u32, u8) {
    // We need to set the clock speed for flash programming to work
    // properly. Reading it out of syscon is less error prone than
    // trying to compile it in at build time

    let syscon = &peripherals.SYSCON;

    let a = syscon.mainclksela.read().bits();
    let b = syscon.mainclkselb.read().bits();
    let div = syscon.ahbclkdiv.read().bits();

    // corresponds to FRO 96 MHz, see 4.5.34 in user manual
    const EXPECTED_MAINCLKSELA: u32 = 3;
    // corresponds to Main Clock A, see 4.5.45 in user manual
    const EXPECTED_MAINCLKSELB: u32 = 0;

    // We expect the 96MHz clock to be used based on the ROM.
    // If it's not there are probably more (bad) surprises coming
    // and panicking is reasonable
    if a != EXPECTED_MAINCLKSELA || b != EXPECTED_MAINCLKSELB {
        panic!();
    }

    if div == 0 {
        (96, div as u8)
    } else {
        (48, div as u8)
    }
}
