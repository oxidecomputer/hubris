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

use crate::app;
use crate::task;
use crate::time::Timestamp;
use crate::umem::USlice;

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
    assert_eq!(prev_task_table, None);
    // Record length as well.
    TASK_TABLE_SIZE = tasks.len();

    // Configure the timer.
    // TODO this is not the right place for this, I snuck it in here for
    // expediency
    // Note that we have *no idea* what our tick frequency is. TODO.
    let syst = &*cortex_m::peripheral::SYST::ptr();
    // Program reload value.
    syst.rvr.write(159_999); // TODO: that's 10ms at 16MHz
    // Clear current value.
    syst.cvr.write(0);
    // Enable counter and interrupt.
    syst.csr.modify(|v| v | 0b111);

    let scb = &*cortex_m::peripheral::SCB::ptr();
    scb.shcsr.modify(|x| x | 0b111 << 16);
}

pub unsafe fn set_irq_table(irqs: &[abi::Interrupt]) {
    let prev_table = core::mem::replace(
        &mut IRQ_TABLE_BASE,
        Some(NonNull::new_unchecked(irqs.as_ptr() as *mut abi::Interrupt)),
    );
    // Catch double-uses of this function.
    assert_eq!(prev_table, None);
    // Record length as well.
    IRQ_TABLE_SIZE = irqs.len();
}

pub fn reinitialize(task: &mut task::Task) {
    task.save = SavedState::default();
    // Modern ARMv7-M machines require 8-byte stack alignment.
    assert!(task.descriptor.initial_stack & 0x7 == 0);

    // The remaining state is stored on the stack.
    // TODO: this assumes availability of an FPU.
    // Use checked operations to get a reference to the exception frame.
    let frame_size = core::mem::size_of::<ExtendedExceptionFrame>();
    let mut uslice: USlice<ExtendedExceptionFrame> = USlice::from_raw(
        task.descriptor.initial_stack as usize - frame_size,
        1,
    )
    .unwrap();
    assert!(task.can_write(&uslice));

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
        // This MPU requires that all regions are 32-byte aligned...in part
        // because it stuffs extra stuff into the bottom five bits.
        debug_assert_eq!(region.base & 0x1F, 0);

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

pub fn start_first_task(task: &task::Task) -> ! {
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
        asm! { "
            msr PSP, $0             @ set the user stack pointer
            ldm $1, {r4-r11}        @ restore the callee-save registers
            svc #0xFF               @ branch into user mode (svc # ignored)
            udf #0xad               @ should not return
        "
            :
            : "r"(task.save.psp),
              "r"(&task.save.r4)
            : "memory", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11"
            : "volatile"
        }
    }
    unreachable!()
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
    asm! {"
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
    asm! {"
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
            .expect("systick irq before kernel started?")
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
    asm! {
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
        4 => panic!("MemManage"),
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

                            // First, disable the interrupt by poking the
                            // Interrupt Clear Enable Register.
                            let nvic = &*cortex_m::peripheral::NVIC::ptr();
                            let reg_num = (irq_num / 32) as usize;
                            let bit_mask = 1 << (irq_num % 32);
                            nvic.icer[reg_num].write(bit_mask);

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
