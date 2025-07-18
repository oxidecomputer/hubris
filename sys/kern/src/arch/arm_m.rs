// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Architecture support for ARMv{6,7,8}-M.
//!
//! # ARM-M timer
//!
//! We use the system tick timer as the kernel timer, but it's only suitable for
//! producing periodic interrupts -- its counter is small and only counts down.
//! So, at each SysTick interrupt, we increment the `TICKS` global that contains
//! the real kernel timestamp. This has the downside that we take regular
//! interrupts to maintain `TICKS`, but has the upside that we don't need
//! special SoC support for timing.
//!
//! # Notes on ARM-M interrupts
//!
//! For performance and (believe it or not) simplicity, this implementation uses
//! several different interrupt service routines:
//!
//! - `SVCall` implements the `SVC` instruction used to make syscalls.
//! - `SysTick` handles interrupts from the System Tick Timer, used to maintain
//! the kernel timestamp.
//! - `PendSV` handles deferred context switches from interrupts.
//!
//! The first two are expected; the last one's a bit odd and deserves an
//! explanation.
//!
//! It has to do with interrupt latency.
//!
//! On any interrupt, the processor stacks a small subset of machine state and
//! then calls our ISR. Our ISR is a normal Rust function, and follows the
//! normal (C) calling convention: there are some registers that it can use
//! without saving, and there are others it must save first. When the ISR
//! returns, it restores any registers it saved.
//!
//! This is great, as long as the code you're returning to is the *same code
//! that called you* -- but in the case of a context switch, it isn't.
//!
//! There's another problem, which is that we'd like to be able to read the
//! values of some of the user registers for syscall arguments and the like...
//! but if we rely on the automatic saving to put them somewhere on the stack,
//! that "somewhere" is opaque and we can't manipulate it.
//!
//! And so, if you want to be able to inspect callee registers (beyond `r0`
//! through `r3`) or switch tasks, you need to do something more elaborate than
//! the basic hardware interrupt behavior: you need to carefully deposit all
//! user state into the `Task`, and then read it back on the way out (possibly
//! from a different `Task` if the context has switched).
//!
//! This is relatively costly, so it's only appropriate to do this in an ISR
//! that you believe will result in a context switch. `SVCall` usually does --
//! our most-used system calls are blocking. `SysTick` usually *does not* -- it
//! will cause a context switch only when it causes a higher-priority timer to
//! fire, which is a sometimes thing. And most hardware interrupt handlers are
//! also not guaranteed to cause a context switch immediately.
//!
//! So, we do the full save/restore sequence around `SVCall` (see the assembly
//! code in that function), but *not* around `SysTick`, and not around other
//! hardware IRQs. Instead, if one of those routines discovers that a context
//! switch is required, it pokes a register that sets the `PendSV` interrupt
//! pending.
//!
//! `PendSV` is intended for this exact use. It will kick in when our ISR exits
//! (i.e. it won't preempt our ISR, but follow it) and perform the full
//! save/restore sequence around invoking the scheduler.
//!
//! We didn't invent this idea -- it's covered in most books on the Cortex-M.
//! We might later decide that most ISRs (including ticks) tend to trigger
//! context switches, and just always do full save/restore, eliminating PendSV.
//! We'll see.

use core::arch::{self, global_asm};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use zerocopy::{FromBytes, Immutable, KnownLayout};

use crate::atomic::AtomicExt;
use crate::descs::RegionAttributes;
use crate::startup::with_task_table;
use crate::task;
use crate::time::Timestamp;
use crate::umem::USlice;
#[cfg(any(armv7m, armv8m))]
use abi::FaultSource;
use abi::{FaultInfo, InterruptNum, UsageError};
#[cfg(armv8m)]
use armv8_m_mpu::{disable_mpu, enable_mpu};
use unwrap_lite::UnwrapLite;

macro_rules! uassert {
    ($cond : expr) => {
        if !$cond {
            panic!("Assertion failed!");
        }
    };
}

/// On ARMvx-M we have to use a global to record the current task pointer, since
/// we don't have a scratch register.
#[no_mangle]
static CURRENT_TASK_PTR: AtomicPtr<task::Task> =
    AtomicPtr::new(core::ptr::null_mut());

/// To allow our clock frequency to be easily determined from a debugger, we
/// store it in memory.
#[no_mangle]
static CLOCK_FREQ_KHZ: AtomicU32 = AtomicU32::new(0);

/// ARMvx-M volatile registers that must be saved across context switches.
#[repr(C)]
#[derive(Debug, Default)]
pub struct SavedState {
    // NOTE: the following fields must be kept contiguous!
    r4: u32,
    r5: u32,
    r6: u32,
    r7: u32,
    r8: u32,
    r9: u32,
    r10: u32,
    r11: u32,
    psp: u32,
    exc_return: u32,

    // gosh it would sure be nice if cfg_if were legal here
    #[cfg(any(armv7m, armv8m))]
    s16: u32,
    #[cfg(any(armv7m, armv8m))]
    s17: u32,
    #[cfg(any(armv7m, armv8m))]
    s18: u32,
    #[cfg(any(armv7m, armv8m))]
    s19: u32,
    #[cfg(any(armv7m, armv8m))]
    s20: u32,
    #[cfg(any(armv7m, armv8m))]
    s21: u32,
    #[cfg(any(armv7m, armv8m))]
    s22: u32,
    #[cfg(any(armv7m, armv8m))]
    s23: u32,
    #[cfg(any(armv7m, armv8m))]
    s24: u32,
    #[cfg(any(armv7m, armv8m))]
    s25: u32,
    #[cfg(any(armv7m, armv8m))]
    s26: u32,
    #[cfg(any(armv7m, armv8m))]
    s27: u32,
    #[cfg(any(armv7m, armv8m))]
    s28: u32,
    #[cfg(any(armv7m, armv8m))]
    s29: u32,
    #[cfg(any(armv7m, armv8m))]
    s30: u32,
    #[cfg(any(armv7m, armv8m))]
    s31: u32,
    // NOTE: the above fields must be kept contiguous!
}

/// Map the volatile registers to (architecture-independent) syscall argument
/// and return slots.
impl task::ArchState for SavedState {
    fn stack_pointer(&self) -> u32 {
        self.psp
    }

    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32 {
        self.r4
    }
    fn arg1(&self) -> u32 {
        self.r5
    }
    fn arg2(&self) -> u32 {
        self.r6
    }
    fn arg3(&self) -> u32 {
        self.r7
    }
    fn arg4(&self) -> u32 {
        self.r8
    }
    fn arg5(&self) -> u32 {
        self.r9
    }
    fn arg6(&self) -> u32 {
        self.r10
    }

    fn syscall_descriptor(&self) -> u32 {
        self.r11
    }

    /// Writes syscall return argument 0.
    fn ret0(&mut self, x: u32) {
        self.r4 = x
    }
    fn ret1(&mut self, x: u32) {
        self.r5 = x
    }
    fn ret2(&mut self, x: u32) {
        self.r6 = x
    }
    fn ret3(&mut self, x: u32) {
        self.r7 = x
    }
    fn ret4(&mut self, x: u32) {
        self.r8 = x
    }
    fn ret5(&mut self, x: u32) {
        self.r9 = x
    }
}

/// Stuff placed on the stack at exception entry whether or not an FPU is
/// present.
#[derive(Debug, FromBytes, Immutable, KnownLayout, Default)]
#[repr(C)]
pub struct BaseExceptionFrame {
    r0: u32,
    r1: u32,
    r2: u32,
    r3: u32,
    r12: u32,
    lr: u32,
    pc: u32,
    xpsr: u32,
}

cfg_if::cfg_if! {
    if #[cfg(any(armv7m, armv8m))] {
        /// Extended version for FPU.
        #[derive(Debug, FromBytes, Immutable, KnownLayout, Default)]
        #[repr(C)]
        pub struct ExtendedExceptionFrame {
            base: BaseExceptionFrame,
            fpu_regs: [u32; 16],
            fpscr: u32,
            reserved: u32,
        }
    } else if #[cfg(armv6m)] {
        /// Wee version for non-FPU.
        #[derive(Debug, FromBytes, Immutable, KnownLayout, Default)]
        #[repr(C)]
        pub struct ExtendedExceptionFrame {
            base: BaseExceptionFrame,
        }
    } else {
        compile_error!("unknown M-profile");
    }
}

/// Initially we just set the Thumb Mode bit, the minimum required.
const INITIAL_PSR: u32 = 1 << 24;

/// We don't really care about the initial FPU mode; 0 is reasonable.
#[cfg(any(armv7m, armv8m))]
const INITIAL_FPSCR: u32 = 0;

/// EXC_RETURN is used on ARMv8m to return from an exception. This value
/// differs between secure and non-secure in two important ways:
/// bit 6 = S = secure or non-secure stack used
/// bit 0 = ES = the security domain the exception was taken to
/// These need to be consistent! The failure mode is a secure fault otherwise.
/// We currently assume that TrustZone has not been enabled (even on the parts
/// that support it) (and that bit 6 and bit 0 can always be set).
const EXC_RETURN_CONST: u32 = 0xFFFFFFED;

// Because debuggers need to know the clock frequency to set the SWO clock
// scaler that enables ITM, and because ITM is particularly useful when
// debugging boot failures, this should be set as early in boot as it can
// be.
pub unsafe fn set_clock_freq(tick_divisor: u32) {
    CLOCK_FREQ_KHZ.store(tick_divisor, Ordering::Relaxed);
}

pub fn reinitialize(task: &mut task::Task) {
    *task.save_mut() = SavedState::default();
    let initial_stack = task.descriptor().initial_stack as usize;

    // Modern ARMvX-M machines require 8-byte stack alignment. Make sure that's
    // still true. Note that this carries the risk of panic on task re-init if
    // the task table is corrupted -- this is deliberate.
    uassert!(initial_stack & 0x7 == 0);

    // The remaining state is stored on the stack.
    // Use checked operations to get a reference to the exception frame.
    let frame_size = core::mem::size_of::<ExtendedExceptionFrame>();
    // The subtract below can overflow if the task table is corrupt -- let's
    // make that failure a little easier to read:
    uassert!(initial_stack >= frame_size);
    // Ok. Generate a uslice for the task's starting stack frame.
    let mut frame_uslice: USlice<ExtendedExceptionFrame> =
        USlice::from_raw(initial_stack - frame_size, 1).unwrap_lite();

    // Before we set our frame, find the region that contains the top word of
    // the stack -- one word below the initial stack pointer -- and zap the
    // region from the base to the stack pointer with a distinct (and storied)
    // pattern.
    //
    // Note that if the initial stack pointer is zero, we use saturating
    // arithmetic and get zero for the top word, which is outside any region and
    // causes this to be skipped. (Not that we expect zero, but we're the kernel
    // and we don't trust tasks.)
    if let Some(region) = task
        .region_table()
        .iter()
        .find(|region| region.contains(initial_stack.saturating_sub(4)))
    {
        // If the slice doesn't fit in the region, this will fail. Should this
        // occur, don't crash the entire system, since this is a diagnostic tool
        // -- just skip filling the stack.
        if let Ok(mut uslice) = USlice::<u32>::from_raw(
            region.base as usize,
            (initial_stack - frame_size - region.base as usize) >> 2,
        ) {
            // This one, we're unwrapping rather than tolerating failure. This
            // is because try_write failing would indicate an invalid region
            // descriptor for the task (read-only stack area) which would bite
            // us later.
            let zap = task.try_write(&mut uslice).unwrap_lite();
            for word in zap.iter_mut() {
                *word = 0xbaddcafe;
            }
        }
    }

    let descriptor = task.descriptor();
    let frame = &mut task.try_write(&mut frame_uslice).unwrap_lite()[0];

    // Conservatively/defensively zero the entire frame.
    *frame = ExtendedExceptionFrame::default();
    // Now fill in the bits we actually care about.
    frame.base.pc = descriptor.entry_point | 1; // for thumb
    frame.base.xpsr = INITIAL_PSR;
    frame.base.lr = 0xFFFF_FFFF; // trap on return from main
    #[cfg(any(armv7m, armv8m))]
    {
        frame.fpscr = INITIAL_FPSCR;
    }

    // Set the initial stack pointer, *not* to the stack top, but to the base of
    // this frame.
    task.save_mut().psp = frame as *const _ as u32;

    // Finally, record the EXC_RETURN we'll use to enter the task.
    task.save_mut().exc_return = EXC_RETURN_CONST;
}

/// PMSAv6/7-style precomputed region data.
///
/// This struct is `repr(C)` to preserve the order of its fields, which happens
/// to match the order of registers in the MPU. While we don't bit-copy the
/// struct directly, this does improve code generation in practice.
#[cfg(any(armv6m, armv7m))]
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct RegionDescExt {
    rbar: u32,
    rasr: u32,
}

#[cfg(any(armv6m, armv7m))]
pub const fn compute_region_extension_data(
    base: u32,
    size: u32,
    attributes: RegionAttributes,
) -> RegionDescExt {
    // This platform requires 32-byte alignment of all regions.
    if base & 0x1F != 0 {
        panic!();
    }

    let ratts = attributes;
    let xn = !ratts.contains(RegionAttributes::EXECUTE);
    // These AP encodings are chosen such that we never deny *privileged*
    // code (i.e. us) access to the memory.
    let ap = if ratts.contains(RegionAttributes::WRITE) {
        0b011
    } else if ratts.contains(RegionAttributes::READ) {
        0b010
    } else {
        0b001
    };
    // Set the TEX/SCB bits to configure memory type, caching policy, and
    // shareability (with other cores or masters). See table B3-13 in the
    // ARMv7-M ARM. (Settings are identical on v6-M but the sharability and
    // TEX bits tend to be ignored.)
    let (tex, scb) = if ratts.contains(RegionAttributes::DEVICE) {
        // Device memory.
        (0b000, 0b001)
    } else if ratts.contains(RegionAttributes::DMA) {
        // Conservative settings for normal memory assuming that DMA might
        // be a problem:
        // - Outer and inner non-cacheable.
        // - Shared.
        (0b001, 0b100)
    } else {
        // Aggressive settings for normal memory assume that it is used only
        // by this processor:
        // - Outer and inner write-back
        // - Read and write allocate.
        // - Not shared.
        (0b001, 0b011)
    };
    // On v6/7-M the MPU expresses size of a region in log2 form _minus
    // one._ So, the minimum allowed size of 32 bytes is represented as 4,
    // because `2**(4 + 1) == 32`.
    //
    // We store sizes in the region table in an architecture-independent
    // form (number of bytes) because it simplifies basically everything
    // else but this routine. Here we must convert between the two -- and
    // quickly, because this is called on every context switch.
    //
    // The image-generation tools check at build time that region sizes are
    // powers of two. So, we can assume that the size has a single 1 bit. We
    // can cheaply compute log2 of this by counting trailing zeroes, but
    // ARMv7-M doesn't have a native instruction for that -- only leading
    // zeroes. The equivalent using leading zeroes is
    //
    //   log2(N) = bits_in_word - 1 - clz(N)
    //
    // Because we want log2 _minus one_ we compute it as...
    //
    //   log2_m1(N) = bits_in_word - 2 - clz(N)
    //
    // If the size is zero or one, this subtraction will underflow. This
    // should not occur in a valid image, but could occur due to runtime
    // flash corruption. Any region size under 32 bytes is illegal on
    // ARMv7-M anyway, so panicking is better than triggering possibly
    // undefined hardware behavior.
    //
    // On ARMv6-M, there is no CLZ instruction either. This winds up
    // generating decent intrinsic code for `leading_zeros` so we'll live
    // with it.
    let l2size = 30 - size.leading_zeros();

    // Region attribute and size register; we enable the region by default
    // because we load it with the MPU off.
    let rasr =
        (xn as u32) << 28 | ap << 24 | tex << 19 | scb << 16 | l2size << 1 | 1;

    // Build the RBAR contents without the VALID bit or region number.
    let rbar = base;
    RegionDescExt { rasr, rbar }
}

#[cfg(any(armv6m, armv7m))]
pub fn apply_memory_protection(task: &task::Task) {
    // We are manufacturing authority to interact with the MPU here, because we
    // can't thread a cortex-specific peripheral through an
    // architecture-independent API. This approach might bear revisiting later.
    let mpu = unsafe {
        // At least by not taking a &mut we're confident we're not violating
        // aliasing....
        &*cortex_m::peripheral::MPU::PTR
    };

    // Turn off the MPU.
    //
    // Safety: this has no actual memory safety implications, except for
    // potentially exposing the kernel to a NULL dereference that succeeds.
    unsafe {
        mpu.ctrl.write(0);
    }

    for (i, region) in task.region_table().iter().enumerate() {
        let data = region.arch_data;
        // With the MPU off, there are no particular constraints on the order in
        // which we write these fields.
        //
        // Safety: we're messing with memory protection, so from the API's point
        // of view this is very unsafe. But we're loading values generated by
        // our (trusted) build script, which only affect tasks and not us. So
        // this should be safe by default.
        unsafe {
            // Select a region.
            mpu.rnr.write(i as u32);
            // Set region base address.
            mpu.rbar.write(data.rbar);
            // Configure the region.
            mpu.rasr.write(data.rasr);
        }
    }

    // Turn MPU back on.
    //
    // Safety: same as above, has no safety implications really.
    unsafe {
        mpu.ctrl.write(0b101);
    }
}

/// ARMv8-M specific MPU accelerator data.
///
/// This is `repr(C)` only to make the field order match the register order in
/// the hardware, which improves code generation. We do not actually rely on the
/// in-memory representation of this struct otherwise.
#[cfg(armv8m)]
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct RegionDescExt {
    /// Contents of the RBAR register.
    rbar: u32,

    /// Contents of the RLAR register.
    rlar: u32,

    /// This region's portion of the four-byte MAIR register.
    mair: u8,
}

#[cfg(armv8m)]
pub const fn compute_region_extension_data(
    base: u32,
    size: u32,
    ratts: RegionAttributes,
) -> RegionDescExt {
    // This MPU requires that all regions are 32-byte aligned...in part
    // because it stuffs extra stuff into the bottom five bits.
    if base & 0x1F != 0 {
        panic!();
    }

    let xn = !ratts.contains(RegionAttributes::EXECUTE);
    // ARMv8m has less granularity than ARMv7m for privilege
    // vs non-privilege so there's no way to say that privilege
    // can be read write but non-privilge can only be read only
    // This _should_ be okay?
    let ap = if ratts.contains(RegionAttributes::WRITE) {
        0b01 // RW by any privilege level
    } else if ratts.contains(RegionAttributes::READ) {
        0b11 // Read only by any privilege level
    } else {
        0b00 // RW by privilege code only
    };

    let (mair, sh) = if ratts.contains(RegionAttributes::DEVICE) {
        // Most restrictive: device memory, outer shared.
        (0b00000000, 0b10)
    } else if ratts.contains(RegionAttributes::DMA) {
        // Outer/inner non-cacheable, outer shared.
        (0b01000100, 0b10)
    } else {
        let rw = (ratts.contains(RegionAttributes::READ) as u8) << 1
            | (ratts.contains(RegionAttributes::WRITE) as u8);
        // write-back transient, not shared
        (0b0100_0100 | rw | rw << 4, 0b00)
    };

    // RLAR = our upper bound. We're going ahead and setting the enable bit
    // because we expect this to be loaded with the MPU _disabled._ Loading this
    // with the MPU _enabled_ would involve momentary inconsistency between RLAR
    // and RBAR, since the two cannot be written simultaneously, and that would
    // be Bad.
    let rlar = (base + size - 32) | 1; // upper bound | enable bit

    // RBAR = the base
    let rbar = (xn as u32)
        | ap << 1
        | (sh as u32) << 3  // sharability
        | base;
    RegionDescExt { rlar, rbar, mair }
}

#[cfg(armv8m)]
pub fn apply_memory_protection(task: &task::Task) {
    let mpu = unsafe {
        // At least by not taking a &mut we're confident we're not violating
        // aliasing....
        &*cortex_m::peripheral::MPU::PTR
    };

    // Disable the MPU before making changes. This is critical to correctness of
    // this function!
    //
    // Because regions consist of several registers, there is no order in which
    // we can update those registers with the MPU _enabled_ that doesn't risk a
    // race condition. MPU updates that load the RBAR from one region and the
    // RLAR from another have caused real crashes.
    //
    // Disabling and re-enabling the MPU is very inexpensive (single-digit
    // cycles) so don't sweat it -- do the correct thing.
    unsafe {
        disable_mpu(mpu);
    }

    // We'll collect the MAIR register contents here. Indices 0-3 correspond to
    // MAIR0's bytes (in LE order); 4-7 are MAIR1.
    let mut mairs = [0; 8];

    for (i, region) in task.region_table().iter().enumerate() {
        let rnr = i as u32;

        let ext = &region.arch_data;

        mairs[i] = ext.mair;

        // Set the attridx field of the RLAR to just choose the attributes with
        // the same index as the region. This lets us treat MAIR as an array
        // corresponding to the regions.
        //
        // We unfortunately can't do this at compile time, because regions can
        // be shared, and may not be used in the same table position in all
        // tasks.
        let rlar = ext.rlar | (i as u32) << 1; // AttrIdx

        unsafe {
            mpu.rnr.write(rnr);
            mpu.rbar.write(ext.rbar);
            mpu.rlar.write(rlar);
        }
    }

    unsafe {
        // Load the MAIR registers.
        mpu.mair[0].write(u32::from_le_bytes(mairs[..4].try_into().unwrap()));
        mpu.mair[1].write(u32::from_le_bytes(mairs[4..].try_into().unwrap()));
        enable_mpu(mpu, true);
    }
}

pub fn start_first_task(tick_divisor: u32, task: &task::Task) -> ! {
    // Enable faults and set fault/exception priorities to reasonable settings.
    // Our goal here is to keep the kernel non-preemptive, which means the
    // kernel entry points (SVCall, PendSV, SysTick, interrupt handlers) must be
    // at one priority level. Fault handlers need to be higher priority,
    // however, so that we can detect faults in the kernel.
    //
    // Safety: this is actually fairly safe. We're purely lowering priorities
    // from their defaults, so it can't cause any surprise preemption or
    // anything. But these operations are `unsafe` in the `cortex_m` crate.
    unsafe {
        let scb = &*cortex_m::peripheral::SCB::PTR;
        // Faults on, on the processors that distinguish faults. This
        // distinguishes the following faults from HardFault:
        //
        // - ARMv7+: MEMFAULT, BUSFAULT, USGFAULT
        // - ARMv8: SECUREFAULT
        cfg_if::cfg_if! {
            if #[cfg(armv7m)] {
                scb.shcsr.modify(|x| x | 0b111 << 16);
            } else if #[cfg(armv8m)] {
                scb.shcsr.modify(|x| x | 0b1111 << 16);
            } else if #[cfg(armv6m)] {
                // This facility is missing.
            } else {
                compile_error!("missing fault setup for ARM profile");
            }
        }

        // Set fault and standard exception priorities.
        cfg_if::cfg_if! {
            if #[cfg(armv6m)] {
                // ARMv6 only has 4 priority levels and no configurable fault
                // priorities. Set priorities of SVCall, SysTick and PendSV to 3
                // (the lowest configurable).
                scb.shpr[0].modify(|x| x | 0b11 << 30);
                scb.shpr[1].modify(|x| x | 0b11 << 22 | 0b11 << 30);
            } else if #[cfg(any(armv7m, armv8m))] {
                // Set priority of Usage, Bus, MemManage to 0 (highest
                // configurable).
                scb.shpr[0].write(0x00);
                scb.shpr[1].write(0x00);
                scb.shpr[2].write(0x00);
                // Set priority of SVCall to 0xFF (lowest configurable).
                scb.shpr[7].write(0xFF);
                // SysTick and PendSV also to 0xFF
                scb.shpr[10].write(0xFF);
                scb.shpr[11].write(0xFF);
            } else {
                compile_error!("missing fault priorities for ARM profile");
            }
        }

        #[cfg(any(armv7m, armv8m))]
        {
            // ARM's default disposition is that division by zero doesn't
            // actually fail, but rather returns 0. (!)  It's unclear how
            // placating this kind of programmatic sloppiness doesn't ultimately
            // end in tears; we explicitly configure ourselves to trap on any
            // divide by zero.
            const DIV_0_TRP: u32 = 1 << 4;
            scb.ccr.modify(|x| x | DIV_0_TRP);
        }

        // Configure the priority of all external interrupts so that they can't
        // preempt the kernel.
        let nvic = &*cortex_m::peripheral::NVIC::PTR;

        cfg_if::cfg_if! {
            if #[cfg(armv6m)] {
                // On ARMv6 there are 8 IPR registers, each containing 4
                // interrupt priorities.  Only 2 bits, stored at bits[7:6], are
                // used for the priority level, giving a range of 0-192 in steps
                // of 64.  Writes to the other bits are ignored, so we just set
                // everything high, i.e.  the lowest priority.  For more
                // information see:
                //
                // ARMv6-M Architecture Reference Manual section B3.4.7
                //
                // Do not believe what the docs for the `cortex_m` crate suggest
                // -- the IPR registers on ARMv6M are 32-bits wide.
                for i in 0..8 {
                    nvic.ipr[i].write(0xFFFF_FFFF);
                }
            } else if #[cfg(any(armv7m, armv8m))] {
                // How many IRQs have we got on ARMv7+? This information is
                // stored in a separate area of the address space, away from the
                // NVIC
                let icb = &*cortex_m::peripheral::ICB::PTR;
                let ictr = icb.ictr.read();
                // This gives interrupt count in blocks of 32, minus 1, so there
                // are always at least 32 interrupts.
                let irq_block_count = (ictr as usize & 0xF) + 1;
                let irq_count = irq_block_count * 32;
                // Blindly poke all the interrupts to 0xFF. IPR registers on
                // ARMv7/8 are modeled as `u8` by `cortex_m`, unlike on ARMv6.
                // We're explicit with the `u8` suffix below to ensure that we
                // notice if this changes.
                for i in 0..irq_count {
                    nvic.ipr[i].write(0xFFu8);
                }
            } else {
                compile_error!("missing IRQ priorities for ARM profile");
            }
        }
    }

    // Safety: this, too, is safe in practice but unsafe in API.
    unsafe {
        // Configure the timer.
        let syst = &*cortex_m::peripheral::SYST::PTR;
        // Program reload value.
        syst.rvr.write(tick_divisor - 1);
        // Clear current value.
        syst.cvr.write(0);
        // Enable counter and interrupt.
        syst.csr.modify(|v| v | 0b111);
    }
    // We are manufacturing authority to interact with the MPU here, because we
    // can't thread a cortex-specific peripheral through an
    // architecture-independent API. This approach might bear revisiting later.
    let mpu = unsafe {
        // At least by not taking a &mut we're confident we're not violating
        // aliasing....
        &*cortex_m::peripheral::MPU::PTR
    };

    const ENABLE: u32 = 0b001;
    const PRIVDEFENA: u32 = 0b100;
    // Safety: this has no memory safety implications. The worst it can do is
    // cause us to fault, which is safe. The register API doesn't know this.
    unsafe {
        mpu.ctrl.write(ENABLE | PRIVDEFENA);
    }

    CURRENT_TASK_PTR.store(task as *const _ as *mut _, Ordering::Relaxed);

    extern "C" {
        // Exposed by the linker script.
        static _stack_base: u32;
    }

    // Safety: this is setting the Main stack pointer (i.e. kernel/interrupt
    // stack pointer) limit register. There are two potential outcomes from
    // this:
    // 1. We proceed without issue because we have not yet overflowed our stack.
    // 2. We take an immediate fault.
    //
    // Both these outcomes are safe, even if the second one is annoying.
    #[cfg(armv8m)]
    unsafe {
        cortex_m::register::msplim::write(
            core::ptr::addr_of!(_stack_base) as u32
        );
    }

    // Safety: this is setting the Process (task) stack pointer, which has no
    // effect _assuming_ this code is running on the Main (kernel) stack.
    unsafe {
        cortex_m::register::psp::write(task.save().psp);
    }

    // Run the final pre-kernel assembly sequence to set up the kernel
    // environment!
    //
    // Our basic goal here is to flip into Handler mode (i.e. interrupt state)
    // so that we can switch Thread mode (not-interrupt state) to unprivileged
    // and running off the Process Stack Pointer. The easiest way to do this on
    // ARM-M is by entering Handler mode by a trap. We use SVC, which we also
    // use for system calls; the SVC entry sequence (also in this file) has code
    // to detect this condition and do kernel startup rather than processing it
    // as a syscall.
    cfg_if::cfg_if! {
        if #[cfg(armv6m)] {
            unsafe {
                arch::asm!("
                    @ restore the callee-save registers
                    ldm r0!, {{r4-r7}}
                    ldm r0, {{r0-r3}}
                    mov r11, r3
                    mov r10, r2
                    mov r9, r1
                    mov r8, r0
                    @ Trap into the kernel.
                    svc #0xFF
                    @ noreturn generates a UDF here in case that should return.
                    ",
                    in("r0") &task.save().r4,
                    options(noreturn),
                )
            }
        } else if #[cfg(any(armv7m, armv8m))] {
            unsafe {
                arch::asm!("
                    @ Restore callee-save registers.
                    ldm {task}, {{r4-r11}}
                    @ Trap into the kernel.
                    svc #0xFF
                    @ noreturn generates a UDF here in case that should return.
                    ",
                    task = in(reg) &task.save().r4,
                    options(noreturn),
                )
            }
        } else {
            compile_error!("missing kernel bootstrap sequence for ARM profile");
        }
    }
}

// Handler that gets linked into the vector table for the Supervisor Call (SVC)
// instruction. (Name is dictated by the `cortex_m` crate.)
cfg_if::cfg_if! {
    // TODO: could shave several cycles off SVC entry with more careful ordering
    // of instructions below, though the precise details depend on how complex
    // of an M-series processor you're targeting -- so I've punted on this for
    // the time being.

    // All the syscall handlers use the same strategy, but the implementation
    // differs on different profile variants.
    //
    // First, we inspect LR, which on exception entry contains bits describing
    // the _previous_ (interrupted) processor state. We can use this to detect
    // if the SVC came from the Main (interrupt) stack. This only happens once,
    // during startup, so we vector to a different routine in this case.
    //
    // We then store the calling task's context into the TCB.
    //
    // Then, we call into `syscall_entry`.
    //
    // After that, we repeat the same steps in the opposite order to restore
    // task context (possibly for a different task!).
    if #[cfg(armv6m)] {
        global_asm!{"
            .section .text.SVCall
            .globl SVCall
            .type SVCall,function
            SVCall:
                @ Inspect LR to figure out the caller's mode.
                mov r0, lr
                ldr r1, =0xFFFFFFF3
                bics r0, r0, r1
                @ Is the call coming from thread mode + main stack, i.e.
                @ from the kernel startup routine?
                cmp r0, #0x8
                @ If so, this is startup; jump ahead. The common case falls
                @ through because branch-not-taken tends to be faster on small
                @ cores.
                beq 1f

                @ store volatile state.
                @ first, get a pointer to the current task.
                ldr r0, =CURRENT_TASK_PTR
                ldr r1, [r0]
                @ now, store volatile registers, plus the PSP, plus LR.
                movs r2, r1
                stm r2!, {{r4-r7}}
                mov r4, r8
                mov r5, r9
                mov r6, r10
                mov r7, r11
                stm r2!, {{r4-r7}}
                mrs r4, PSP
                mov r5, lr
                stm r2!, {{r4, r5}}

                @ syscall number is passed in r11. Move it into r0 to pass
                @ it as an argument to the handler, then call the handler.
                mov r0, r11
                bl syscall_entry

                @ we're returning back to *some* task, maybe not the same one.
                ldr r0, =CURRENT_TASK_PTR
                ldr r0, [r0]
                @ restore volatile registers, plus PSP. We will do this in
                @ slightly reversed order for efficiency. First, do the high
                @ ones.
                movs r1, r0
                adds r1, r1, #(4 * 4)
                ldm r1!, {{r4-r7}}
                mov r11, r7
                mov r10, r6
                mov r9, r5
                mov r8, r4
                ldm r1!, {{r4, r5}}
                msr PSP, r4
                mov lr, r5

                @ Now that we no longer need r4-r7 as temporary registers,
                @ restore them too.
                ldm r0!, {{r4-r7}}

                @ resume
                bx lr

            1:  @ starting up the first task.
                @ Drop privilege in Thread mode.
                movs r0, #1
                msr CONTROL, r0
                @ note: no barrier here because exc return serves as barrier

                @ Manufacture a new EXC_RETURN to change the processor mode
                @ when we return.
                ldr r0, ={exc_return}
                mov lr, r0
                bx lr                   @ branch into user mode
        ",
        exc_return = const EXC_RETURN_CONST,
        }
    } else if #[cfg(any(armv7m, armv8m))] {
        global_asm!{"
            .section .text.SVCall
            .globl SVCall
            .type SVCall,function
            SVCall:
                @ Inspect LR to figure out the caller's mode.
                mov r0, lr
                mov r1, #0xFFFFFFF3
                bic r0, r1
                @ Is the call coming from thread mode + main stack, i.e.
                @ from the kernel startup routine?
                cmp r0, #0x8
                @ If so, this is startup; jump ahead. The common case falls
                @ through because branch-not-taken tends to be faster on small
                @ cores.
                beq 1f

                @ store volatile state.
                @ first, get a pointer to the current task.
                movw r0, #:lower16:CURRENT_TASK_PTR
                movt r0, #:upper16:CURRENT_TASK_PTR
                ldr r1, [r0]
                movs r2, r1
                @ fetch the process-mode stack pointer.
                @ fetching into r12 means the order in the stm below is right.
                mrs r12, PSP
                @ now, store volatile registers, plus the PSP in r12, plus LR.
                stm r2!, {{r4-r12, lr}}
                vstm r2, {{s16-s31}}

                @ syscall number is passed in r11. Move it into r0 to pass it as
                @ an argument to the handler, then call the handler.
                movs r0, r11
                bl syscall_entry

                @ we're returning back to *some* task, maybe not the same one.
                movw r0, #:lower16:CURRENT_TASK_PTR
                movt r0, #:upper16:CURRENT_TASK_PTR
                ldr r0, [r0]
                @ restore volatile registers, plus load PSP into r12
                ldm r0!, {{r4-r12, lr}}
                vldm r0, {{s16-s31}}
                msr PSP, r12

                @ resume
                bx lr

            1:  @ starting up the first task.
                movs r0, #1         @ get bitmask to...
                msr CONTROL, r0     @ ...shed privs from thread mode.
                                    @ note: now barrier here because exc return
                                    @ serves as barrier

                mov lr, {exc_return}    @ materialize EXC_RETURN value to
                                        @ return into thread mode, PSP, FP on

                bx lr                   @ branch into user mode
            ",
            exc_return = const EXC_RETURN_CONST,
        }
    } else {
        compile_error!("missing SVCall impl for ARM profile.");
    }
}

/// Records the address of `task` as the current user task.
///
/// # Safety
///
/// This records a pointer that aliases `task`. As long as you don't read that
/// pointer while you have access to `task`, and as long as the `task` being
/// stored is actually in the task table, you'll be okay.
pub unsafe fn set_current_task(task: &task::Task) {
    CURRENT_TASK_PTR.store(task as *const _ as *mut _, Ordering::Relaxed);
    crate::profiling::event_context_switch(task as *const _ as usize);
}

/// Reads the tick counter.
pub fn now() -> Timestamp {
    // Recall that we expect the systick interrupt cannot preempt kernel code,
    // so we're safe to read this in two nonatomic parts here.
    Timestamp::from([
        TICKS[0].load(Ordering::Relaxed),
        TICKS[1].load(Ordering::Relaxed),
    ])
}

/// Kernel global for tracking the current timestamp, measured in ticks.
///
/// This is a pair of `AtomicU32` because (1) we want the interior mutability of
/// the atomic types but (2) ARMv7-M doesn't have any 64-bit atomic operations.
/// We access this only from contexts where we can't be preempted, so, the fact
/// that it's split across two words is ok.
///
/// `TICKS[0]` is the least significant part, `TICKS[1]` the most significant.
static TICKS: [AtomicU32; 2] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU32 = AtomicU32::new(0);
    [ZERO; 2]
};

/// Handler that gets linked into the vector table for the System Tick Timer
/// overflow interrupt. (Name is dictated by the `cortex_m` crate.)
#[allow(non_snake_case)]
#[no_mangle]
pub unsafe extern "C" fn SysTick() {
    crate::profiling::event_timer_isr_enter();
    with_task_table(|tasks| {
        // Load the time before this tick event.
        let t0 = TICKS[0].load(Ordering::Relaxed);
        let t1 = TICKS[1].load(Ordering::Relaxed);

        // Advance the kernel's notion of time by adding 1. Laboriously.
        let (t0, t1) = if let Some(t0p) = t0.checked_add(1) {
            // Incrementing t0 did not roll over, no need to update t1.
            TICKS[0].store(t0p, Ordering::Relaxed);
            (t0p, t1)
        } else {
            // Incrementing t0 overflowed. We need to also increment t1. We use
            // normal checked addition for this, not wrapping, because this
            // should not be able to overflow under normal operation, and would
            // almost certainly indicate state corruption that we'd like to
            // discover.
            TICKS[0].store(0, Ordering::Relaxed);
            TICKS[1].store(t1 + 1, Ordering::Relaxed);
            (0, t1 + 1)
        };

        // Process any timers.
        let now = Timestamp::from([t0, t1]);
        let switch = task::process_timers(tasks, now);

        // If any timers fired, we need to defer a context switch, because the entry
        // sequence to this ISR doesn't save state correctly for efficiency.
        if switch != task::NextTask::Same {
            pend_context_switch_from_isr();
        }
    });
    crate::profiling::event_timer_isr_exit();
}

fn pend_context_switch_from_isr() {
    // This sets the bit to pend a PendSV interrupt. PendSV will happen after
    // the current ISR (and any chained ISRs) returns, and perform the context
    // switch.
    cortex_m::peripheral::SCB::set_pendsv();
}

cfg_if::cfg_if! {
    if #[cfg(armv6m)] {
        global_asm!{"
            .section .text.PendSV
            .globl PendSV
            .type PendSV,function
            PendSV:
                @ store volatile state.
                @ first, get a pointer to the current task.
                ldr r0, =CURRENT_TASK_PTR
                ldr r1, [r0]
                @ now, store volatile registers, plus the PSP, plus LR.
                stm r1!, {{r4-r7}}
                mov r4, r8
                mov r5, r9
                mov r6, r10
                mov r7, r11
                stm r1!, {{r4-r7}}
                mrs r4, PSP
                mov r5, lr
                stm r1!, {{r4, r5}}

                bl pendsv_entry

                @ we're returning back to *some* task, maybe not the same one.
                ldr r0, =CURRENT_TASK_PTR
                ldr r0, [r0]
                @ restore volatile registers, plus PSP. We will do this in
                @ slightly reversed order for efficiency. First, do the high
                @ ones.
                movs r1, r0
                adds r1, r1, #(4 * 4)
                ldm r1!, {{r4-r7}}
                mov r11, r7
                mov r10, r6
                mov r9, r5
                mov r8, r4
                ldm r1!, {{r4, r5}}
                msr PSP, r4
                mov lr, r5

                @ Now that we no longer need r4-r7 as temporary registers,
                @ restore them too.
                ldm r0!, {{r4-r7}}

                @ resume
                bx lr
            ",
        }
    } else if #[cfg(any(armv7m, armv8m))] {
        global_asm!{"
            .section .text.PendSV
            .globl PendSV
            .type PendSV,function
            PendSV:
                @ store volatile state.
                @ first, get a pointer to the current task.
                movw r0, #:lower16:CURRENT_TASK_PTR
                movt r0, #:upper16:CURRENT_TASK_PTR
                ldr r1, [r0]
                @ fetch the process-mode stack pointer.
                @ fetching into r12 means the order in the stm below is right.
                mrs r12, PSP
                @ now, store volatile registers, plus the PSP in r12, plus LR.
                stm r1!, {{r4-r12, lr}}
                vstm r1, {{s16-s31}}

                bl pendsv_entry

                @ we're returning back to *some* task, maybe not the same one.
                movw r0, #:lower16:CURRENT_TASK_PTR
                movt r0, #:upper16:CURRENT_TASK_PTR
                ldr r0, [r0]
                @ restore volatile registers, plus load PSP into r12
                ldm r0!, {{r4-r12, lr}}
                vldm r0, {{s16-s31}}
                msr PSP, r12

                @ resume
                bx lr
            ",
        }
    } else {
        compile_error!("missing PendSV impl for ARM profile.");
    }
}

/// The Rust side of the PendSV handler, after all volatile registers have been
/// saved somewhere predictable.
#[no_mangle]
unsafe extern "C" fn pendsv_entry() {
    crate::profiling::event_secondary_syscall_enter();

    let current = CURRENT_TASK_PTR.load(Ordering::Relaxed);
    uassert!(!current.is_null()); // irq before kernel started?

    // Safety: we're dereferencing the current task pointer, which we're
    // trusting the rest of this module to maintain correctly.
    let current = usize::from(unsafe { (*current).descriptor().index });

    with_task_table(|tasks| {
        let next = task::select(current, tasks);
        apply_memory_protection(next);
        // Safety: next comes from the task table and we don't use it again
        // until next kernel entry, so we meet set_current_task's requirements.
        unsafe {
            set_current_task(next);
        }
    });
    crate::profiling::event_secondary_syscall_exit();
}

#[allow(non_snake_case)]
#[no_mangle]
pub unsafe extern "C" fn DefaultHandler() {
    crate::profiling::event_isr_enter();
    // We can cheaply get the identity of the interrupt that called us from the
    // bottom 9 bits of IPSR.
    //
    // Safety: we're just reading the PSR.
    let exception_num = unsafe {
        let mut ipsr: u32;
        arch::asm!(
            "mrs {}, IPSR",
            out(reg) ipsr,
            options(pure, nomem, preserves_flags, nostack),
        );
        ipsr & 0x1FF
    };

    // The first 16 exceptions are architecturally defined; vendor hardware
    // interrupts start at 16.
    match exception_num {
        // 1=Reset is not handled this way
        2 => panic!("NMI"),
        // 3=HardFault is handled elsewhere
        // 4=MemManage is handled below
        // 5=BusFault is handled below
        // 6=UsageFault is handled below
        // 7-10 are currently reserved
        // 11=SVCall is handled above by its own handler
        12 => panic!("DebugMon"),
        // 13 is currently reserved
        // 14=PendSV is handled above by its own handler
        // 15=SysTick is handled above by its own handler
        x if x >= 16 => {
            // Hardware interrupt
            let irq_num = exception_num - 16;
            let owner = crate::startup::HUBRIS_IRQ_TASK_LOOKUP
                .get(abi::InterruptNum(irq_num))
                .unwrap_or_else(|| panic!("unhandled IRQ {irq_num}"));

            let switch = with_task_table(|tasks| {
                // This can only fail if the IRQ number is out of range, which
                // in this case would mean the hardware is conspiring against
                // us. So ignore it to ensure we don't generate a bogus check.
                disable_irq(irq_num, false).ok();

                // Now, post the notification and return the
                // scheduling hint.
                let n = task::NotificationSet(owner.notification);
                tasks[owner.task as usize].post(n)
            });
            if switch {
                pend_context_switch_from_isr()
            }
        }

        _ => panic!("unknown exception {exception_num}"),
    }
    crate::profiling::event_isr_exit();
}

pub fn disable_irq(n: u32, also_clear_pending: bool) -> Result<(), UsageError> {
    // Disable the interrupt by poking the Interrupt Clear Enable Register.
    let nvic = unsafe { &*cortex_m::peripheral::NVIC::PTR };
    let reg_num = (n / 32) as usize;
    let bit_mask = 1 << (n % 32);
    unsafe {
        nvic.icer
            .get(reg_num)
            .ok_or(UsageError::NoIrq)?
            .write(bit_mask);
    }
    if also_clear_pending {
        unsafe {
            nvic.icpr
                .get(reg_num)
                .ok_or(UsageError::NoIrq)?
                .write(bit_mask);
        }
    }
    Ok(())
}

pub fn enable_irq(n: u32, also_clear_pending: bool) -> Result<(), UsageError> {
    // Enable the interrupt by poking the Interrupt Set Enable Register.
    let nvic = unsafe { &*cortex_m::peripheral::NVIC::PTR };
    let reg_num = (n / 32) as usize;
    let bit_mask = 1 << (n % 32);
    if also_clear_pending {
        // Do this _before_ enabling.
        unsafe {
            nvic.icpr
                .get(reg_num)
                .ok_or(UsageError::NoIrq)?
                .write(bit_mask);
        }
    }
    unsafe {
        nvic.iser
            .get(reg_num)
            .ok_or(UsageError::NoIrq)?
            .write(bit_mask);
    }
    Ok(())
}

/// Looks up an interrupt in the NVIC and returns a cross-platform
/// representation of that interrupt's status.
pub fn irq_status(n: u32) -> Result<abi::IrqStatus, UsageError> {
    let mut status = abi::IrqStatus::empty();

    let nvic = unsafe { &*cortex_m::peripheral::NVIC::PTR };
    let reg_num = (n / 32) as usize;
    let bit_mask = 1 << (n % 32);

    // See if the interrupt is enabled by checking the bit in the Interrupt Set
    // Enable Register.
    let iser_reg = nvic.iser.get(reg_num).ok_or(UsageError::NoIrq)?;
    let enabled = iser_reg.read() & bit_mask == bit_mask;
    status.set(abi::IrqStatus::ENABLED, enabled);

    // See if the interrupt is pending by checking the bit in the Interrupt
    // Set Pending Register (ISPR).
    let pending = nvic.ispr[reg_num].read() & bit_mask == bit_mask;
    status.set(abi::IrqStatus::PENDING, pending);

    Ok(status)
}

pub fn pend_software_irq(
    InterruptNum(n): InterruptNum,
) -> Result<(), UsageError> {
    let nvic = unsafe { &*cortex_m::peripheral::NVIC::PTR };
    let reg_num = (n / 32) as usize;
    let bit_mask = 1 << (n % 32);

    // Pend the IRQ by poking the corresponding bit in the Interrupt Set Pending
    // Register (ISPR).
    let ispr_reg = nvic.ispr.get(reg_num).ok_or(UsageError::NoIrq)?;
    unsafe { ispr_reg.write(bit_mask) };
    Ok(())
}

#[repr(u8)]
#[allow(dead_code)]
#[cfg(any(armv7m, armv8m))]
enum FaultType {
    MemoryManagement = 4,
    BusFault = 5,
    UsageFault = 6,
}

#[cfg(any(armv7m, armv8m))]
global_asm! {"
    .section .text.im_dead
    .globl im_dead
    .type im_dead,function
    .cpu cortex-m4  @ least common denominator we support
    im_dead:
        @ lie down try not to cry cry a lot
        movw r0, #0xed0c
        movt r0, #0xe000
        movw r1, #0x0004
        movt r1, #0x05fa
        str.w  r1, [r0]
    1:
        b 1b


    .section .text.configurable_fault
    .globl configurable_fault
    .type configurable_fault,function
    .cpu cortex-m4  @ least common denominator we support
    configurable_fault:
        @ Read the current task pointer.
        movw r0, #:lower16:CURRENT_TASK_PTR
        movt r0, #:upper16:CURRENT_TASK_PTR
        ldr r0, [r0]
        mrs r12, PSP

        @ Now, to aid those who will debug what induced this fault, save our
        @ context.  Some of our context (namely, r0-r3, r12, LR, the return
        @ address and the xPSR) is already on our stack as part of the fault;
        @ we'll store our remaining registers, plus the PSP (now in r12), plus
        @ exc_return (now in LR) into the save region in the current task.
        @ Note that we explicitly refrain from saving the floating point
        @ registers here:  touching the floating point registers will induce
        @ a lazy save on the stack, which is clearly bad news if we have
        @ overflowed our stack!  We do want to ultimately save them to aid
        @ debuggability, however, so we pass the address to which they should
        @ be saved to our fault handler, which will take the necessary
        @ measures to save them safely.  Finally, note that deferring the
        @ save to later in handle_fault assumes that the floating point
        @ registers are not in fact touched before determmining the fault type
        @ and disabling lazy saving accordingly; should that assumption not
        @ hold, we will need to be (ironically?) less lazy about disabling
        @ lazy saving...
        mov r2, r0
        stm r2!, {{r4-r12, lr}}

        @ Pull our fault number out of IPSR, allowing for program text to be
        @ shared across all configurable faults.  (Note that the exception
        @ number is the bottom 9 bits, but we need only look at the bottom 4
        @ bits as this handler is only used for exceptions with numbers less
        @ than 16.)
        mrs r1, IPSR
        and r1, r1, #0xf
        bl handle_fault

        @ Our task has changed; reload it.
        movw r0, #:lower16:CURRENT_TASK_PTR
        movt r0, #:upper16:CURRENT_TASK_PTR
        ldr r0, [r0]

        @ Restore volatile registers, plus load PSP into r12
        ldm r0!, {{r4-r12, lr}}
        vldm r0, {{s16-s31}}
        msr PSP, r12

        @ resume
        bx lr

    .section .text.MemoryManagement
    .globl MemoryManagement
    .type MemoryManagement,function
    MemoryManagement:
        b configurable_fault

    .section .text.BusFault
    .globl BusFault
    .type BusFault,function
    BusFault:
        b configurable_fault

    .section .text.UsageFault
    .globl UsageFault
    .type UsageFault,function
    UsageFault:
        b configurable_fault

    .section .text.HardFault
    .globl HardFault
    .type HardFault,function
    HardFault:
        b im_dead
    ",
}

#[cfg(armv6m)]
global_asm! {"
    .section .text.HardFault
    .globl HardFault
    .type HardFault,function
    HardFault:
        @ Read the current task pointer.
        ldr r0, =CURRENT_TASK_PTR
        ldr r0, [r0]
        mrs r12, PSP

        @ Now, to aid those who will debug what induced this fault, save our
        @ context.  Some of our context (namely, r0-r3, r12, LR, the return
        @ address and the xPSR) is already on our stack as part of the fault;
        @ we'll store our remaining registers, plus the PSP, plus exc_return
        @ (now in LR) into the save region in the current task.
        mov r2, r0
        stm r2!, {{r4-r7}}
        mov r4, r8
        mov r5, r9
        mov r6, r10
        mov r7, r11
        stm r2!, {{r4-r7}}
        mrs r4, PSP
        mov r5, lr
        stm r2!, {{r4, r5}}

        bl handle_fault

        @ Our task has changed; reload it.
        ldr r0, =CURRENT_TASK_PTR
        ldr r0, [r0]
        @ restore volatile registers, plus PSP. We will do this in
        @ slightly reversed order for efficiency. First, do the high
        @ ones.
        movs r1, r0
        adds r1, r1, #(4 * 4)
        ldm r1!, {{r4-r7}}
        mov r11, r7
        mov r10, r6
        mov r9, r5
        mov r8, r4
        ldm r1!, {{r4, r5}}
        msr PSP, r4
        mov lr, r5

        @ Now that we no longer need r4-r7 as temporary registers,
        @ restore them too.
        ldm r0!, {{r4-r7}}

        @ resume
        bx lr
    ",
}

bitflags::bitflags! {
    /// Bits in the Configurable Fault Status Register.
    #[repr(transparent)]
    struct Cfsr: u32 {
        // Bits 0-7: MMFSR (Memory Management Fault Status Register)
        const IACCVIOL = 1 << 0;
        const DACCVIOL = 1 << 1;
        // MMFSR bit 2 reserved
        const MUNSTKERR = 1 << 3;
        const MSTKERR = 1 << 4;
        const MLSPERR = 1 << 5;
        // MMFSR bit 6 reserved
        const MMARVALID = 1 << 7;

        // Bits 8-15: BFSR (Bus Fault Status Register)
        const IBUSERR = 1 << (8 + 0);
        const PRECISERR = 1 << (8 + 1);
        const IMPRECISERR = 1 << (8 + 2);
        const UNSTKERR = 1 << (8 + 3);
        const STKERR = 1 << (8 + 4);
        const LSPERR = 1 << (8 + 5);
        // BFSR bit 6 reserved
        const BFARVALID = 1 << (8 + 7);

        // Bits 16-31: UFSR (Usage Fault Status Register)
        const UNDEFINSTR = 1 << (16 + 0);
        const INVSTATE = 1 << (16 + 1);
        const INVPC = 1 << (16 + 2);
        const NOCP = 1 << (16 + 3);

        #[cfg(armv8m)]
        const STKOF = 1 << (16 + 4);

        // UFSR bits 4-7 reserved on ARMv7-M -- 5-7 on ARMv8-M
        const UNALIGNED = 1 << (16 + 8);
        const DIVBYZERO = 1 << (16 + 9);

        // UFSR bits 10-31 reserved
    }
}

/// Rust entry point for fault.
///
/// # Safety
///
/// In brief: don't call this. This is an implementation factor of the fault
/// handler assembly code and should not be used for other purposes.
#[no_mangle]
#[cfg(armv6m)]
unsafe extern "C" fn handle_fault(task: *mut task::Task) {
    // Who faulted?
    let (from_thread_mode, idx) = {
        // Safety: we're dereferencing the task pointer, because we trust the
        // assembly fault handler to pass us a legitimate one. We use it
        // immediately and discard it because otherwise it would alias the task
        // table below.
        let t = unsafe { &(*task) };
        (
            t.save().exc_return & 0b1000 != 0,
            usize::from(t.descriptor().index),
        )
    };

    if !from_thread_mode {
        // Uh. This fault originates from the kernel. We don't get fault
        // information on ARMv6M, so we're just printing:
        panic!("Kernel fault");
    }

    // Okay, now that we're confident we came from a task, we need to deal with
    // the case where the fault occurred while stacking an SVC exception frame.
    // In this case, the SVC exception will still be set as pending, which means
    // when we try to return to the supervisor to handle this fault, it'll
    // generate a spurious SVC. Even in the best of cases, this breaks the
    // supervisor.
    //
    // It would be super great if there were, say, a register in the System
    // Control Block that would tell us that SVC is pended, wouldn't it? Perhaps
    // it could be called the System Handler Control and State Register. In
    // fact, if you read the ARMv6-M ARM, you will find a register with such a
    // name, and might be tempted to use it!
    //
    // BEWARE.
    //
    // In a _different section_ of that manual, there is a throwaway footnote
    // that reads:
    //
    // > The DWT, BPU, ROM table, DCB, and the SHCSR and DFSR registers are
    // > accessible through the DAP interface. Access from the processor is
    // > IMPLEMENTATION DEFINED.
    //
    // On the Cortex-M0+, this very attractive register works great from the
    // debugger but _reads as zero from the kernel._ Ugh.
    //
    // Instead, we are using the always-active ICSR register, which lets us
    // _detect_ the pending SVC _but not clear it._ To clear it, we use the
    // mitigation mechanism defined over in syscalls.rs.
    //
    // The case where an SVC is pending in ICSR uniquely identifies a task
    // having faulted during SVC, because a hardfault _in the kernel_ during
    // processing of an SVC would not have made it here (see above).
    {
        let scb = unsafe { &*cortex_m::peripheral::SCB::PTR };
        let icsr = scb.icsr.read();
        // VECTPENDING is 9 bits, so, why are we casting it to a u8? Because
        // this code is ARMv6-M specific, and ARMv6-M is architecturally
        // specified as having no more than 32 interrupts (plus 16 exceptions).
        let vectpending = (icsr >> 12) as u8;

        // If we're in a hardfault (which we know, because it's the only fault
        // on ARMv6M and we are in a fault handler) and an SVC is pending...
        if vectpending == 11 {
            crate::syscalls::EXPECT_PHANTOM_SYSCALL
                .store(true, core::sync::atomic::Ordering::Relaxed);
        }
    }

    // ARMv6-M, to reduce complexity, does not distinguish fault causes.
    let fault = FaultInfo::InvalidOperation(0);

    // We are now going to force a fault on our current task and directly
    // switch to a task to run.
    with_task_table(|tasks| {
        let next = match task::force_fault(tasks, idx, fault) {
            task::NextTask::Specific(i) => &tasks[i],
            task::NextTask::Other => task::select(idx, tasks),
            task::NextTask::Same => &tasks[idx],
        };

        if core::ptr::eq(next as *const _, task as *const _) {
            panic!("attempt to return to Task #{idx} after fault");
        }

        apply_memory_protection(next);
        // Safety: next comes from the task table and we don't use it again
        // until next kernel entry, so we meet set_current_task's requirements.
        unsafe {
            set_current_task(next);
        }
    });
}

pub fn reset() -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}

/// Common implementation of fault handling.
///
/// # Safety
///
/// Requirements for using this safely include:
///
/// - Call this on the way into the kernel from a (naked) ISR, not from within
///   the kernel Rust code.
/// - Ensure that `task` is a pointer to an initialized, aligned Task in the
///   task table.
/// - Ensure that `fpsave` points to that task's floating point save area.
#[no_mangle]
#[cfg(any(armv7m, armv8m))]
unsafe extern "C" fn handle_fault(
    task: *mut task::Task,
    fault_type: FaultType,
    fpsave: *mut u32,
) {
    // To diagnose the fault, we're going to need access to the System Control
    // Block. Pull such access from thin air.
    //
    // Safety: this is dereferencing the raw pointer produced by SCB::ptr. We
    // trust that the returned pointer is valid (non-null, aligned). The
    // resulting reference is to a static-scoped Sync thing, and it's a shared
    // reference, so we shouldn't be breaking any rules by doing this. Arguably
    // this should be available as a safe operation in the cortex_m crate, but
    // that crate comes with _ideas_ about peripheral ownership management.
    let scb = unsafe { &*cortex_m::peripheral::SCB::PTR };
    let cfsr = Cfsr::from_bits_truncate(scb.cfsr.read());

    // Who faulted? Collect some parameters from the task.
    //
    // Safety: we're dereferencing the raw `task` pointer passed in. Our
    // contract requires that it be valid. We immediately throw away the result
    // of dereferencing it, as it would otherwise alias the task table obtained
    // later.
    let (exc_return, psp, idx) = unsafe {
        let t = &(*task);
        (
            t.save().exc_return,
            t.save().psp,
            usize::from(t.descriptor().index),
        )
    };
    let from_thread_mode = exc_return & 0b1000 != 0;

    if !from_thread_mode {
        // Uh. This fault originates from the kernel. Let's try to make the
        // panic as clear and as information-rich as possible, while trying
        // to not consume unnecessary program text (i.e., it isn't worth
        // conditionally printing MMFAR or BFAR only on a MemoryManagement
        // fault or a BusFault, respectively).  In that vein, note that we
        // promote our fault type to a u32 to not pull in the Display trait
        // for either FaultType or u8.
        panic!(
            "Kernel fault {}: \
            CFSR={:#010x}, MMFAR={:#010x}, BFAR={:#010x}",
            (fault_type as u8) as u32,
            cfsr.bits(),
            scb.mmfar.read(),
            scb.bfar.read(),
        );
    }

    // Okay, now that we're confident we came from a task, we need to deal with
    // the case where the fault is **derived.** In ARMvX-M jargon, a derived
    // fault is one produced by attempting to handle a different exception or
    // fault. In our case these are almost always due to mishandling of the
    // stack by the task, e.g.
    //
    // - Making a syscall (SVC) without enough stack space for the exception
    //   frame,
    // - Setting your stack pointer to NULL and then taking an interrupt or
    //   fault, or
    // - Dereferencing NULL, or executing an illegal instruction, without enough
    //   stack.
    //
    // In these cases, we'll wind up taking a MemManage fault, but the original
    // exception from which it was derived (SVC, Bus, Usage, etc) will still be
    // set to *pending* in the interrupt hardware. This means that after we
    // handle the fault, when we try to return-from-interrupt into the
    // supervisor task, we'll still try to handle the pended exception.
    //
    // This will appear as though _the supervisor_ has called it, generating a
    // phantom fault or syscall. This breaks things.
    //
    // This only affects architectural exceptions/faults and not hardware
    // interrupts, which we _do_ want to process even if a fault occurred. The
    // pended bits for those exceptions/faults are in the System Handler Control
    // and State Register, bits 15:12. We need to clear them. We do this
    // unconditionally because it doesn't hurt, and it's slightly
    // faster/smaller.
    //
    // Safety: the cortex-m crate makes all these registers blanket-unsafe
    // without documenting the required preconditions. From the ARMv7-M spec, we
    // can infer that the main risk here is if SVC were higher priority than
    // this handler, which it is not.
    unsafe {
        scb.shcsr.modify(|bits| bits & !(0b1111 << 12));
    }

    let (fault, stackinvalid) = match fault_type {
        FaultType::MemoryManagement => {
            if cfsr.contains(Cfsr::MSTKERR) {
                // If we have an MSTKERR, we know very little other than the
                // fact that the user's stack pointer is so trashed that we
                // can't store through it.  (In particular, we seem to have no
                // way at getting at our faulted PC.)
                (FaultInfo::StackOverflow { address: psp }, true)
            } else if cfsr.contains(Cfsr::IACCVIOL) {
                (FaultInfo::IllegalText, false)
            } else {
                (
                    FaultInfo::MemoryAccess {
                        address: if cfsr.contains(Cfsr::MMARVALID) {
                            Some(scb.mmfar.read())
                        } else {
                            None
                        },
                        source: FaultSource::User,
                    },
                    false,
                )
            }
        }

        FaultType::BusFault => (
            FaultInfo::BusError {
                address: if cfsr.contains(Cfsr::BFARVALID) {
                    Some(scb.bfar.read())
                } else {
                    None
                },
                source: FaultSource::User,
            },
            false,
        ),

        FaultType::UsageFault => (
            if cfsr.contains(Cfsr::DIVBYZERO) {
                FaultInfo::DivideByZero
            } else if cfsr.contains(Cfsr::UNDEFINSTR) {
                FaultInfo::IllegalInstruction
            } else {
                FaultInfo::InvalidOperation(cfsr.bits())
            },
            false,
        ),
    };

    // Because we are responsible for clearing all conditions, we write back
    // the value of CFSR that we read
    //
    // Safety: this is a traditional write-one-to-clear register that, when
    // written, clears recorded fault states. It is not at _all_ clear why its
    // write function is unsafe.
    unsafe {
        scb.cfsr.write(cfsr.bits());
    }

    if stackinvalid {
        // We know that we have an invalid stack; to prevent our subsequent
        // save of the dead task's floating point registers from storing
        // floating point registers to the invalid stack, we explicitly clear
        // the Lazy Stack Preservation Active bit in our Floating Point
        // Context Control register.
        const LSPACT: u32 = 1 << 0;
        unsafe {
            let fpu = &*cortex_m::peripheral::FPU::PTR;
            fpu.fpccr.modify(|x| x & !LSPACT);
        }
    }

    // It's safe to store our floating point registers; store them now to
    // preserve as much state as possible for debugging.
    //
    // Safety: asm! is always unsafe, obvs, but in this case as long as fpsave
    // points to a correctly aligned area large enough to store 16 floats -- a
    // property our caller is required to ensure -- this is ok.
    unsafe {
        arch::asm!("vstm {0}, {{s16-s31}}", in(reg) fpsave);
    }

    // We are now going to force a fault on our current task and directly
    // switch to a task to run.  (It may be tempting to use PendSV here,
    // but that won't work on ARMv8-M in the presence of MPU faults on
    // PSP:  even with PendSV pending, ARMv8-M will generate a MUNSTKERR
    // when returning from an exception with a PSP that generates an MPU
    // fault!)
    with_task_table(|tasks| {
        let next = match task::force_fault(tasks, idx, fault) {
            task::NextTask::Specific(i) => &tasks[i],
            task::NextTask::Other => task::select(idx, tasks),
            task::NextTask::Same => &tasks[idx],
        };

        if core::ptr::eq(next as *const _, task as *const _) {
            panic!("attempt to return to Task #{idx} after fault");
        }

        apply_memory_protection(next);
        // Safety: this leaks a pointer aliasing next into static scope, but
        // we're not going to read it back until the next kernel entry, so we
        // won't be aliasing/racing.
        unsafe {
            set_current_task(next);
        }
    });
}

cfg_if::cfg_if! {
    if #[cfg(armv6m)] {
        // The ARMv6M atomic operations are implemented by disabling interrupts
        // globally. In a normal configuration the kernel arranges priorities so
        // that it's never preempted, making this moot. However, it's entirely
        // possible for an application to adjust interrupt priorities to support
        // custom low-latency interrupt service routines that don't go through
        // the kernel; disabling interrupts here ensures that we remain correct
        // in the presence of such code.
        //
        // Note that the routines that take an `ordering` are always used with
        // constant values -- despite being `inline(never)`, the compiler winds
        // up specializing these routines to the constant ordering, which is
        // what we want. The `inline(never)` helps to avoid code explosion,
        // which on constrained M0s is usually more important than syscall
        // entry/exit speed.

        impl AtomicExt for AtomicBool {
            type Primitive = bool;

            #[inline(never)]
            fn swap_polyfill(&self, value: Self::Primitive, ordering: Ordering)
                -> Self::Primitive
            {
                let (lo, so) = rmw_ordering(ordering);
                cortex_m::interrupt::free(|_| {
                    let prev = self.load(lo);
                    self.store(value, so);
                    prev
                })
            }
        }

        /// Translates an ordering suppled to a read-modify-write operation into
        /// the distinct orderings implied for its load and store phases,
        /// respectively.
        ///
        /// This mapping is described using informal language in the docs for
        /// the `core::sync::atomic::Ordering` type.
        ///
        /// This is `inline(always)` because its `o` argument is _basically
        /// always_ a literal, causing its code to evaporate at compile time.
        #[inline(always)]
        fn rmw_ordering(o: Ordering) -> (Ordering, Ordering) {
            match o {
                Ordering::AcqRel => (Ordering::Acquire, Ordering::Release),
                Ordering::Relaxed => (o, o),
                Ordering::SeqCst => (o, o),
                Ordering::Acquire => (Ordering::Acquire, Ordering::Relaxed),
                Ordering::Release => (Ordering::Relaxed, Ordering::Release),
                // Other orderings are not suitable for RMW operations.
                _ => panic!(),
            }
        }
    } else {
        impl AtomicExt for AtomicBool {
            type Primitive = bool;

            #[inline(always)]
            fn swap_polyfill(&self, value: Self::Primitive, ordering: Ordering)
                -> Self::Primitive
            {
                self.swap(value, ordering)
            }
        }

    }
}
