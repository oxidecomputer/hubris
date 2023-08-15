// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![feature(naked_functions)]

#[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
mod dice;
#[cfg(feature = "dice-mfg")]
mod dice_mfg_usart;
mod images;
#[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
use lpc55_puf::Puf;

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

// Execute this function before jumping to Hubris.
// This function will panic if PUF is not in the desired state.
#[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
fn puf_check(puf: &lpc55_pac::PUF) {
    use crate::dice::KEY_INDEX;
    let puf = Puf::new(puf);

    if !puf.is_index_blocked(KEY_INDEX) || !puf.is_index_locked(KEY_INDEX) {
        panic!();
    }
}

fn enable_debug(peripherals: &lpc55_pac::Peripherals) {
    const HUBRIS_DEBUG_CRED_BEACON: u32 = 10000;

    let beacon = peripherals.SYSCON.debug_auth_beacon.read().bits();
    // The beacon is made up of two parts: 16 bits of credential beacon
    // which is signed by the root key and 16 bits of authentication beacon
    // which can be changed at runtime. Use the sign credential beacon
    // to decide if debugging should be enabled
    let cred_beacon = beacon & 0xffff;

    if cred_beacon == HUBRIS_DEBUG_CRED_BEACON {
        // See Section 4.5.83 of UM11126, this isn't actually named
        // in the manual apart from `CPU0 SWD-AP: 0x12345678`
        const SWD_MAGIC: u32 = 0x12345678;
        // This register is not defined in the LPC55 PAC
        // This information comes from Section 4.5.83 of UM11126
        const SYSCON_SWDCPU0: u32 = 0x4000_0FB4;
        // Enable SWD port access for CPU0
        // SAFETY: writing anything other than the defined magic will lock
        // out debug access which is the behavior we want here!
        unsafe {
            core::ptr::write_volatile(SYSCON_SWDCPU0 as *mut u32, SWD_MAGIC);
        }
        // The debug auth code will set the `SYSCON_DEBUG_FEATURES` but not
        // the `DP` variant. We'll trust the debug auth code to have set
        // the options we want
        let debug_features = peripherals.SYSCON.debug_features.read().bits();
        peripherals
            .SYSCON
            .debug_features_dp
            .write(|w| unsafe { w.bits(debug_features) });

        // Prevent futher changes to the debug settings
        peripherals
            .SYSCON
            .debug_lock_en
            .write(|w| w.lock_all().enable());
    }
}

/// Run the common startup routine for LPC55-based roots of trust.
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

    nuke_stack();

    #[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
    puf_check(&peripherals.PUF);

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

    // This is purposely done as the very last step after all validation
    // and secret clearing has happened
    enable_debug(&peripherals);
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

/// Destroys the contents of the unused portion of the stack.
///
/// # Safety
///
/// This routine is not marked as `unsafe` because, if you're doing entirely
/// safe things, it's not possible to shoot yourself in the foot with this.
///
/// However, if you're doing something weird, you totally can, so keep reading.
///
/// To use this correctly, you must be sure that the current program is not
/// using any references to memory between the current stack pointer (on entry
/// to this routine) and the `_stack_base` symbol generated by the build system.
///
/// This is _automatically_ ensured in normal programs, which have no way to
/// refer to stack they haven't yet used. So in most cases, you can satisfy this
/// contract trivially.
///
/// However, if you're doing something weird with unused stack memory, be very
/// careful.
#[naked]
extern "C" fn nuke_stack() {
    extern "C" {
        static _stack_base: u32;
    }

    // Theory of operation:
    //
    // ARM uses what's called (by ARM) a "full descending" stack pointer. That
    // means that (1) the word pointed to by the stack pointer has valid data in
    // it, and (2) when things are pushed onto the stack, the stack pointer is
    // decremented.
    //
    // So, our goal is to trash every word in memory between _stack_base and SP,
    // **exclusive.** We cannot trash the word at SP because it's in use by the
    // calling routine.
    //
    // We explicitly nuke the contents of the caller-save registers r0-3 here.
    // We do not nuke the callee-save registers, because to do so we'd be
    // obligated to save their contents, which would defeat the point of
    // overwriting them.
    //
    // However, we do not use the stack ourselves, nor do we use the callee-save
    // registers, so we don't save them anywhere.
    unsafe {
        core::arch::asm!("
            ldr r0, ={stack_base}   @ Get limit into r0
            mov r1, sp              @ Get current sp into r1 for convenience
            mov r2, #0              @ Get a zero into r2
            mov r3, #0              @ Also zero r3 for good measure

        0:  cmp r1, r0              @ are we done?
            beq 1f                  @ if so, break

            str r2, [r1, #-4]!      @ Store a zero just below r1 and decrement
            b 0b                    @ repeat

        1:  bx lr                   @ all done
            ",
            stack_base = sym _stack_base,
            options(noreturn)
        )
    }
}
