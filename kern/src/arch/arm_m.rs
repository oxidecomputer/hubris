//! Architecture support for ARMv{7,8}-M.
//!
//! Mostly ARMv7-M at the moment.
//!
//! # ARM-M timer
//!
//! We use the system tick timer as the kernel timer, but it's only suitable for
//! producing periodic interrupts -- its counter is small and only counts down.
//! So, at each interrupt, we increment the `TICKS` global that contains the
//! real kernel timestamp. This has the downside that we take regular interrupts
//! to maintain `TICKS`, but has the upside that we don't need special SoC
//! support for timing.
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

use core::ptr::NonNull;

use zerocopy::FromBytes;

use abi::{FaultSource, FaultInfo};
use crate::app;
use crate::task;
use crate::time::Timestamp;
use crate::umem::USlice;

/// Log things from kernel context. This macro is made visible to the rest of
/// the kernel by a chain of `#[macro_use]` attributes, but its implementation
/// is very architecture-specific at the moment.
///
/// At the moment, there are two (architecture-specific) ways to log:  via
/// semihosting (configured via the "klog-semihosting" feature) or via the
/// ARM's Instrumentation Trace Macrocell (configured via the "klog-itm"
/// feature).  If neither of these features is enabled, klog! will be stubbed
/// out.
///
/// In the future, we will likely want to add at least one more mechanism for
/// logging (one that can be presumably be made neutral with respect to
/// architecure), whereby kernel logs can be produced somewhere (e.g., a ring
/// buffer) from which they can be consumed by some entity for shipping
/// elsewhere.
///
#[cfg(not(any(feature = "klog-semihosting", feature = "klog-itm")))]
macro_rules! klog {
    ($s:expr) => { };
    ($s:expr, $($tt:tt)*) => { };
}

#[cfg(feature = "klog-itm")]
macro_rules! klog {
    ($s:expr) => {
        #[allow(unused_unsafe)]
        unsafe {
            let stim = &mut (*cortex_m::peripheral::ITM::ptr()).stim[0];
            cortex_m::iprintln!(stim, $s);
        }
    };
    ($s:expr, $($tt:tt)*) => {
        #[allow(unused_unsafe)]
        unsafe {
            let stim = &mut (*cortex_m::peripheral::ITM::ptr()).stim[0];
            cortex_m::iprintln!(stim, $s, $($tt)*);
        }
    };
}

#[cfg(feature = "klog-semihosting")]
macro_rules! klog {
    ($s:expr) => { let _ = cortex_m_semihosting::hprintln!($s); };
    ($s:expr, $($tt:tt)*) => { let _ = cortex_m_semihosting::hprintln!($s, $($tt)*); };
}

macro_rules! uassert {
    ($cond : expr) => {
        if ! $cond {
            panic!("Assertion failed!");
        }
    }
}

macro_rules! uassert_eq {
    ($cond1 : expr, $cond2 : expr) => {
        if ! ($cond1 == $cond2) {
            panic!("Assertion failed!");
        }
    }
}

/// On ARMvx-M we use a global to record the task table position and extent.
#[no_mangle]
static mut TASK_TABLE_BASE: Option<NonNull<task::Task>> = None;
#[no_mangle]
static mut TASK_TABLE_SIZE: usize = 0;

/// On ARMvx-M we use a global to record the interrupt table position and extent.
#[no_mangle]
static mut IRQ_TABLE_BASE: Option<NonNull<abi::Interrupt>> = None;
#[no_mangle]
static mut IRQ_TABLE_SIZE: usize = 0;

/// On ARMvx-M we have to use a global to record the current task pointer, since
/// we don't have a scratch register.
#[no_mangle]
static mut CURRENT_TASK_PTR: Option<NonNull<task::Task>> = None;

/// ARMvx-M volatile registers that must be saved across context switches.
///
/// TODO: this set is a great start but is missing half the FPU registers.
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
#[derive(Debug, FromBytes, Default)]
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

/// Extended version for FPU.
#[derive(Debug, FromBytes, Default)]
#[repr(C)]
pub struct ExtendedExceptionFrame {
    base: BaseExceptionFrame,
    fpu_regs: [u32; 16],
    fpscr: u32,
    reserved: u32,
}

/// Initially we just set the Thumb Mode bit, the minimum required.
const INITIAL_PSR: u32 = 1 << 24;

/// We don't really care about the initial FPU mode; 0 is reasonable.
const INITIAL_FPSCR: u32 = 0;

/// Records `tasks` as the system-wide task table.
///
/// If a task table has already been set, panics.
///
/// # Safety
///
/// This stashes a copy of `tasks` without revoking your right to access it,
/// which is a potential aliasing violation if you call `with_task_table`. So
/// don't do that. The normal kernel entry sequences avoid this issue.
pub unsafe fn set_task_table(tasks: &mut [task::Task]) {
    let prev_task_table = core::mem::replace(
        &mut TASK_TABLE_BASE,
        Some(NonNull::from(&mut tasks[0])),
    );
    // Catch double-uses of this function.
    uassert_eq!(prev_task_table, None);
    // Record length as well.
    TASK_TABLE_SIZE = tasks.len();
}

pub unsafe fn set_irq_table(irqs: &[abi::Interrupt]) {
    let prev_table = core::mem::replace(
        &mut IRQ_TABLE_BASE,
        Some(NonNull::new_unchecked(irqs.as_ptr() as *mut abi::Interrupt)),
    );
    // Catch double-uses of this function.
    uassert_eq!(prev_table, None);
    // Record length as well.
    IRQ_TABLE_SIZE = irqs.len();
}

pub fn reinitialize(task: &mut task::Task) {
    task.save = SavedState::default();
    // Modern ARMv7-M machines require 8-byte stack alignment.
    // TODO: it is a little rude to assert this in an operation that can be used
    // after boot... but we do want to ensure that this condition holds...
    uassert!(task.descriptor.initial_stack & 0x7 == 0);

    // The remaining state is stored on the stack.
    // TODO: this assumes availability of an FPU.
    // Use checked operations to get a reference to the exception frame.
    let frame_size = core::mem::size_of::<ExtendedExceptionFrame>();
    let mut uslice: USlice<ExtendedExceptionFrame> = USlice::from_raw(
        task.descriptor.initial_stack as usize - frame_size,
        1,
    )
    .unwrap();
    uassert!(task.can_write(&uslice));

    let frame = unsafe { &mut uslice.assume_writable()[0] };

    // Conservatively/defensively zero the entire frame.
    *frame = ExtendedExceptionFrame::default();
    // Now fill in the bits we actually care about.
    frame.base.pc = task.descriptor.entry_point | 1; // for thumb
    frame.base.xpsr = INITIAL_PSR;
    frame.base.lr = 0xFFFF_FFFF; // trap on return from main
    frame.fpscr = INITIAL_FPSCR;

    // Set the initial stack pointer, *not* to the stack top, but to the base of
    // this frame.
    task.save.psp = frame as *const _ as u32;

    // Finally, record the EXC_RETURN we'll use to enter the task.
    // TODO: this assumes floating point is in use.
    task.save.exc_return = 0xFFFFFFED;
}

#[cfg(armv7m)]
pub fn apply_memory_protection(task: &task::Task) {
    // We are manufacturing authority to interact with the MPU here, because we
    // can't thread a cortex-specific peripheral through an
    // architecture-independent API. This approach might bear revisiting later.
    let mpu = unsafe {
        // At least by not taking a &mut we're confident we're not violating
        // aliasing....
        &*cortex_m::peripheral::MPU::ptr()
    };

    for (i, region) in task.region_table.iter().enumerate() {
        let rbar = (i as u32)  // region number
            | (1 << 4)  // honor the region number
            | region.base;
        let ratts = region.attributes;
        let xn = !ratts.contains(app::RegionAttributes::EXECUTE);
        // These AP encodings are chosen such that we never deny *privileged*
        // code (i.e. us) access to the memory.
        let ap = if ratts.contains(app::RegionAttributes::WRITE) {
            0b011
        } else if ratts.contains(app::RegionAttributes::READ) {
            0b010
        } else {
            0b001
        };
        let (tex, scb) = if ratts.contains(app::RegionAttributes::DEVICE) {
            (0b000, 0b111)
        } else {
            (0b001, 0b111)
        };
        // This is a bit of a hack; it works if the size is a power of two, but
        // will undersize the region if it isn't. We really need to validate the
        // regions at boot time with architecture-specific logic....
        let l2size = 30 - region.size.leading_zeros();
        let rasr = (xn as u32) << 28
            | ap << 24
            | tex << 19
            | scb << 16
            | l2size << 1
            | (1 << 0); // enable
        unsafe {
            mpu.rbar.write(rbar);
            mpu.rasr.write(rasr);
        }
    }
}

#[cfg(armv8m)]
pub fn apply_memory_protection(task: &task::Task) {
    // Sigh cortex-m crate doesn't have armv8-m support
    // Let's poke it manually to make sure we're doing this right..
    let mpu = unsafe {
        // At least by not taking a &mut we're confident we're not violating
        // aliasing....
        &*cortex_m::peripheral::MPU::ptr()
    };
    unsafe {
        const DISABLE: u32 = 0b000;
        const PRIVDEFENA: u32 = 0b100;
        // From the ARMv8m MPU manual
        //
        // Any outstanding memory transactions must be forced to complete by
        // executing a DMB instruction and the MPU disabled before it can be
        // configured
        cortex_m::asm::dmb();
        mpu.ctrl.write(DISABLE | PRIVDEFENA);
    }

    for (i, region) in task.region_table.iter().enumerate() {
        // This MPU requires that all regions are 32-byte aligned...in part
        // because it stuffs extra stuff into the bottom five bits.
        debug_assert_eq!(region.base & 0x1F, 0);

        let rnr = i as u32;

        let ratts = region.attributes;
        let xn = !ratts.contains(app::RegionAttributes::EXECUTE);
        // ARMv8m has less granularity than ARMv7m for privilege
        // vs non-privilege so there's no way to say that privilege
        // can be read write but non-privilge can only be read only
        // This _should_ be okay?
        let ap = if ratts.contains(app::RegionAttributes::WRITE) {
            0b01 // RW by any privilege level
        } else if ratts.contains(app::RegionAttributes::READ) {
            0b11 // Read only by any privilege level
        } else {
            0b00 // RW by privilege code only
        };

        // Keep this as the most restrictive for devices and least
        // restrictive for memory right now.
        let mair = if ratts.contains(app::RegionAttributes::DEVICE) {
            0b00000000
        } else {
            0b11111111
        };

        // Sharability for normal memory. This is ignored for device memory
        // Keep this as outer sharable on the safe side for devices (think
        // about this more later)
        let sh = 0b10;

        // RLAR = our upper bound
        let rlar = region.base + region.size
                | (i as u32) << 1 // AttrIndx
                | (1 << 0); // enable

        // RBAR = the base
        let rbar = (xn as u32)
            | ap << 1
            | (sh as u32) << 3  // sharability
            | region.base;

        unsafe {
            // RNR
            core::ptr::write_volatile(0xe000_ed98 as *mut u32, rnr);
            // MAIR
            if rnr < 4 {
                let mut mair0 = (0xe000_edc0 as *const u32).read_volatile();
                mair0 = mair0 | (mair as u32) << (rnr * 8);
                core::ptr::write_volatile(0xe000_edc0 as *mut u32, mair0);
            } else {
                let mut mair1 = (0xe000_edc4 as *const u32).read_volatile();
                mair1 = mair1 | (mair as u32) << ((rnr - 4) * 8);
                core::ptr::write_volatile(0xe000_edc4 as *mut u32, mair1);
            }
            // RBAR
            core::ptr::write_volatile(0xe000_ed9c as *mut u32, rbar);
            // RLAR
            core::ptr::write_volatile(0xe000_eda0 as *mut u32, rlar);
        }
    }

    unsafe {
        const ENABLE: u32 = 0b001;
        const PRIVDEFENA: u32 = 0b100;
        mpu.ctrl.write(ENABLE | PRIVDEFENA);
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


}

pub fn start_first_task(task: &task::Task) -> ! {
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
        let scb = &*cortex_m::peripheral::SCB::ptr();
        // Faults on.
        //
        // This enables MEMFAULT, BUSFAULT, USGFAULT, SECUREFAULT (ARMv8m)
        #[cfg(armv7m)]
        {
            scb.shcsr.modify(|x| x | 0b111 << 16);
        }
        #[cfg(armv8m)]
        {
            scb.shcsr.modify(|x| x | 0b1111 << 16);
        }
        // Set priority of Usage, Bus, MemManage to 0 (highest configurable).
        scb.shpr[0].write(0x00);
        scb.shpr[1].write(0x00);
        scb.shpr[2].write(0x00);
        // Set priority of SVCall to 0xFF (lowest configurable).
        scb.shpr[7].write(0xFF);
        // SysTick and PendSV also to 0xFF
        scb.shpr[10].write(0xFF);
        scb.shpr[11].write(0xFF);

        // Now, force all external interrupts to 0xFF too, so they can't preempt
        // the kernel.
        let nvic = &*cortex_m::peripheral::NVIC::ptr();
        // How many interrupts have we got? This information is stored in a
        // separate area of the address space, away from the NVIC, and is
        // (presumably due to an oversight) not present in the cortex_m API, so
        // let's fake it.
        let ictr = (0xe000_e004 as *const u32).read_volatile();
        // This gives interrupt count in blocks of 32.
        let irq_block_count = ictr as usize & 0xF;
        let irq_count = irq_block_count * 32;
        // Blindly poke all the interrupts to 0xFF.
        for i in 0..irq_count {
            nvic.ipr[i].write(0xFF);
        }
    }

    // Safety: this, too, is safe in practice but unsafe in API.
    unsafe {
        // Configure the timer.
        // Note that we have *no idea* what our tick frequency is. TODO.
        let syst = &*cortex_m::peripheral::SYST::ptr();
        // Program reload value.
        syst.rvr.write(159_999); // TODO: that's 10ms at 16MHz
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
        &*cortex_m::peripheral::MPU::ptr()
    };

    const ENABLE: u32 = 0b001;
    const PRIVDEFENA: u32 = 0b100;
    // Safety: this has no memory safety implications. The worst it can do is
    // cause us to fault, which is safe. The register API doesn't know this.
    unsafe {
        mpu.ctrl.write(ENABLE | PRIVDEFENA);
    }

    unsafe {
        CURRENT_TASK_PTR = Some(NonNull::from(task));
    }

    unsafe {
        llvm_asm! { "
            msr PSP, $0             @ set the user stack pointer
            ldm $1, {r4-r11}        @ restore the callee-save registers
            svc #0xFF               @ branch into user mode (svc # ignored)
            udf #0xad               @ should not return
        "
            :
            : "r"(task.save.psp),
              "r"(&task.save.r4)
            : "memory"
            : "volatile"
        }
        core::hint::unreachable_unchecked()
    }
}

/// Handler that gets linked into the vector table for the Supervisor Call (SVC)
/// instruction. (Name is dictated by the `cortex_m` crate.)
#[allow(non_snake_case)]
#[naked]
#[no_mangle]
pub unsafe extern "C" fn SVCall() {
    // TODO: could shave several cycles off SVC entry with more careful ordering
    // of instructions below, though the precise details depend on how complex
    // of an M-series processor you're targeting -- so I've punted on this for
    // the time being.
    llvm_asm! {"
        cmp lr, #0xFFFFFFF9     @ is it coming from inside the kernel?
        beq 1f                  @ if so, we're starting the first task;
                                @ jump ahead.
        @ the common case is handled by branch-not-taken as it's faster

        @ store volatile state.
        @ first, get a pointer to the current task.
        movw r0, #:lower16:CURRENT_TASK_PTR
        movt r0, #:upper16:CURRENT_TASK_PTR
        ldr r1, [r0]
        @ fetch the process-mode stack pointer.
        @ fetching into r12 means the order in the stm below is right.
        mrs r12, PSP
        @ now, store volatile registers, plus the PSP in r12, plus LR.
        stm r1, {r4-r12, lr}

        @ syscall number is passed in r11. Move it into r0 to pass it as an
        @ argument to the handler, then call the handler.
        movs r0, r11
        bl syscall_entry

        @ we're returning back to *some* task, maybe not the same one.
        movw r0, #:lower16:CURRENT_TASK_PTR
        movt r0, #:upper16:CURRENT_TASK_PTR
        ldr r0, [r0]
        @ restore volatile registers, plus load PSP into r12
        ldm r0, {r4-r12, lr}
        msr PSP, r12

        @ resume
        bx lr

    1:  @ starting up the first task.
        movs r0, #1             @ get bitmask to...
        msr CONTROL, r0         @ ...shed privs from thread mode.
                                @ note: now barrier here because exc return
                                @ serves as barrier

        mov lr, #0xFFFFFFED     @ materialize EXC_RETURN value to
                                @ return into thread mode, PSP, FP on

        bx lr                   @ branch into user mode
        "
        :
        :
        :
        : "volatile"
    }
}

/// Manufacture a mutable/exclusive reference to the task table from thin air
/// and hand it to `body`. This bypasses borrow checking and should only be used
/// at kernel entry points, then passed around.
///
/// Because the lifetime of the reference passed into `body` is anonymous, the
/// reference can't easily be stored, which is deliberate.
///
/// # Safety
///
/// You can use this safely at kernel entry points, exactly once, to create a
/// reference to the task table.
pub unsafe fn with_task_table<R>(body: impl FnOnce(&mut [task::Task]) -> R) -> R{
    let tasks = core::slice::from_raw_parts_mut(
        TASK_TABLE_BASE.expect("kernel not started").as_mut(),
        TASK_TABLE_SIZE,
    );
    body(tasks)
}

/// Manufacture a shared reference to the interrupt action table from thin air
/// and hand it to `body`. This bypasses borrow checking and should only be used
/// at kernel entry points, then passed around.
///
/// Because the lifetime of the reference passed into `body` is anonymous, the
/// reference can't easily be stored, which is deliberate.
pub fn with_irq_table<R>(body: impl FnOnce(&[abi::Interrupt]) -> R) -> R{
    // Safety: as long as a legit pointer was stored in IRQ_TABLE_BASE, or no
    // pointer has been stored, we can do this safely.
    let table = unsafe {
        core::slice::from_raw_parts(
            IRQ_TABLE_BASE.expect("kernel not started").as_ptr(),
            IRQ_TABLE_SIZE,
        )
    };
    body(table)
}

/// Records the address of `task` as the current user task.
///
/// # Safety
///
/// This records a pointer that aliases `task`. As long as you don't read that
/// pointer except at syscall entry, you'll be okay.
pub unsafe fn set_current_task(task: &mut task::Task) {
    CURRENT_TASK_PTR = Some(NonNull::from(task));
}

/// Reads the tick counter.
pub fn now() -> Timestamp {
    Timestamp::from(unsafe { TICKS })
}

/// Kernel global for tracking the current timestamp, measured in ticks.
///
/// This is a mutable `u64` instead of an `AtomicU64` because ARMv7-M doesn't
/// have any 64-bit atomic operations. So, we access it carefully from
/// non-preemptible contexts.
static mut TICKS: u64 = 0;

/// Handler that gets linked into the vector table for the System Tick Timer
/// overflow interrupt. (Name is dictated by the `cortex_m` crate.)
#[allow(non_snake_case)]
#[no_mangle]
pub unsafe extern "C" fn SysTick() {
    // We configure this interrupt to have the same priority as SVC, which means
    // there's no way this can preempt the kernel -- it will only preempt user
    // code. As a result, we can manufacture exclusive references to various
    // bits of kernel state.
    let ticks = &mut TICKS;
    with_task_table(|tasks| safe_sys_tick_handler(ticks, tasks));
}

/// The meat of the systick handler, after we do the unsafe things.
fn safe_sys_tick_handler(ticks: &mut u64, tasks: &mut [task::Task]) {
    // Advance the kernel's notion of time, then give up the ability to
    // accidentally do it again.
    *ticks += 1;
    let now = Timestamp::from(*ticks);
    drop(ticks);

    // Process any timers.
    let switch = task::process_timers(tasks, now);

    // If any timers fired, we need to defer a context switch, because the entry
    // sequence to this ISR doesn't save state correctly for efficiency.
    if switch != task::NextTask::Same {
        pend_context_switch_from_isr();
    }
}

fn pend_context_switch_from_isr() {
    // This sets the bit to pend a PendSV interrupt. PendSV will happen after
    // the current ISR (and any chained ISRs) returns, and perform the context
    // switch.
    cortex_m::peripheral::SCB::set_pendsv();
}

#[allow(non_snake_case)]
#[naked]
#[no_mangle]
pub unsafe extern "C" fn PendSV() {
    llvm_asm! {"
        @ store volatile state.
        @ first, get a pointer to the current task.
        movw r0, #:lower16:CURRENT_TASK_PTR
        movt r0, #:upper16:CURRENT_TASK_PTR
        ldr r1, [r0]
        @ fetch the process-mode stack pointer.
        @ fetching into r12 means the order in the stm below is right.
        mrs r12, PSP
        @ now, store volatile registers, plus the PSP in r12, plus LR.
        stm r1, {r4-r12, lr}

        @ syscall number is passed in r11. Move it into r0 to pass it as an
        @ argument to the handler, then call the handler.
        bl pendsv_entry

        @ we're returning back to *some* task, maybe not the same one.
        movw r0, #:lower16:CURRENT_TASK_PTR
        movt r0, #:upper16:CURRENT_TASK_PTR
        ldr r0, [r0]
        @ restore volatile registers, plus load PSP into r12
        ldm r0, {r4-r12, lr}
        msr PSP, r12

        @ resume
        bx lr
        "
        :
        :
        :
        : "volatile"
    }
}

/// The Rust side of the PendSV handler, after all volatile registers have been
/// saved somewhere predictable.
#[no_mangle]
unsafe extern "C" fn pendsv_entry() {
    with_task_table(|tasks| {
        let current = CURRENT_TASK_PTR
            .expect("irq before kernel started?")
            .as_ptr();
        let idx = (current as usize - tasks.as_ptr() as usize)
            / core::mem::size_of::<task::Task>();

        let next = task::select(idx, tasks);
        let next = &mut tasks[next];
        apply_memory_protection(next);
        set_current_task(next);
    });
}

#[allow(non_snake_case)]
#[no_mangle]
pub unsafe extern "C" fn DefaultHandler() {
    // We can cheaply get the identity of the interrupt that called us from the
    // bottom 9 bits of IPSR.
    let mut ipsr: u32;
    llvm_asm! {
        "mrs $0, IPSR"
        : "=r"(ipsr)
    }
    let exception_num = ipsr & 0x1FF;

    // The first 16 exceptions are architecturally defined; vendor hardware
    // interrupts start at 16.
    match exception_num {
        // 1=Reset is not handled this way
        2 => panic!("NMI"),
        // 3=HardFault is handled elsewhere
        // 4=MemManage is handled below
        5 => panic!("BusFault"),
        6 => panic!("UsageFault"),
        // 7-10 are currently reserved
        // 11=SVCall is handled above by its own handler
        12 => panic!("DebugMon"),
        // 13 is currently reserved
        // 14=PendSV is handled above by its own handler
        // 15=SysTick is handled above by its own handler

        x if x > 16 => {
            // Hardware interrupt
            let irq_num = exception_num - 16;
            let switch = with_task_table(|tasks| {
                with_irq_table(|irqs| {
                    for entry in irqs {
                        if entry.irq == irq_num {
                            // Early exit on the first (and should be sole)
                            // match.

                            disable_irq(irq_num);

                            // Now, post the notification and return the
                            // scheduling hint.
                            let n = task::NotificationSet(entry.notification);
                            return Ok(tasks[entry.task as usize].post(n));
                        }
                    }
                    Err(())
                })
            });
            match switch {
                Ok(true) => pend_context_switch_from_isr(),
                Ok(false) => (),
                Err(_) => panic!("unhandled IRQ {}", irq_num),
            }
        }

        _ => panic!("unknown exception {}", exception_num),
    }
}

pub fn disable_irq(n: u32) {
    // Disable the interrupt by poking the Interrupt Clear Enable Register.
    let nvic = unsafe { &*cortex_m::peripheral::NVIC::ptr() };
    let reg_num = (n / 32) as usize;
    let bit_mask = 1 << (n % 32);
    unsafe {
        nvic.icer[reg_num].write(bit_mask);
    }
}

pub fn enable_irq(n: u32) {
    // Enable the interrupt by poking the Interrupt Set Enable Register.
    let nvic = unsafe { &*cortex_m::peripheral::NVIC::ptr() };
    let reg_num = (n / 32) as usize;
    let bit_mask = 1 << (n % 32);
    unsafe {
        nvic.iser[reg_num].write(bit_mask);
    }
}

/// Initial entry point for handling a memory management fault.
#[allow(non_snake_case)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn MemoryManagement() {
    llvm_asm! { "
        @ Get the exc_return value into an argument register, which is
        @ difficult to do from higher-level code.
        mov r0, lr
        @ While we're being unsafe, go ahead and read the current task pointer.
        movw r1, #:lower16:CURRENT_TASK_PTR
        movt r1, #:upper16:CURRENT_TASK_PTR
        ldr r1, [r1]
        b mem_manage_fault
        "
        ::::"volatile"
    }
}

bitflags::bitflags! {
    /// Bits in the Memory Management Fault Status Register.
    #[repr(transparent)]
    struct Mmfsr: u8 {
        const IACCVIOL = 1 << 0;
        const DACCVIOL = 1 << 1;
        // bit 2 reserved
        const MUNSTKERR = 1 << 3;
        const MSTKERR = 1 << 4;
        const MLSPERR = 1 << 5;
        // bit 6 reserved
        const MMARVALID = 1 << 7;
    }
}

/// Rust entry point for memory management fault.
#[allow(non_snake_case)]
#[no_mangle]
unsafe extern "C" fn mem_manage_fault(exc_return: u32, task: *mut task::Task) {
    // To diagnose the fault, we're going to need access to the System Control
    // Block. Pull such access from thin air.
    let scb = &*cortex_m::peripheral::SCB::ptr();

    // Who faulted?
    let from_thread_mode = exc_return & 0b1000 != 0;
    // What did they do? MemManage status is in bits 7:0 of the Configurable
    // Fault Status Register.
    let mmfsr = Mmfsr::from_bits_truncate(scb.cfsr.read() as u8);
    // Where did they do it? Faulting address in MMFAR (when available).
    let mmfar = scb.mmfar.read();

    if from_thread_mode {
        // Build up a FaultInfo record describing what we know.
        let address = if mmfsr.contains(Mmfsr::MMARVALID) { Some(mmfar) } else { None };
        let fault = FaultInfo::MemoryAccess {
            address,
            source: FaultSource::User,
        };
        with_task_table(|tasks| {
            let idx = (task as usize - tasks.as_ptr() as usize)
                / core::mem::size_of::<task::Task>();
            // Ignore the scheduling hint -- we aren't able to forward it to the
            // PendSV routine anyway.
            let _ = task::force_fault(tasks, idx, fault);
        });
        pend_context_switch_from_isr();
    } else {
        // Uh. This fault originates from the kernel. Let's try to make the
        // panic as clear as possible.
        panic!("Memory management fault in kernel mode\nMMFSR = {:?}\nMMFAR = 0x{:08x}",
            mmfsr, mmfar);
    }
}

