use core::ptr::NonNull;

use zerocopy::FromBytes;

use crate::app;
use crate::task;
use crate::umem::USlice;

/// On ARMvx-M we use a global to record the task table position and extent.
#[no_mangle]
static mut TASK_TABLE_BASE: Option<NonNull<task::Task>> = None;
#[no_mangle]
static mut TASK_TABLE_SIZE: usize = 0;

/// On ARMvx-M we have to use a global to record the current task pointer, since
/// we don't have a scratch register.
#[no_mangle]
static mut CURRENT_TASK_PTR: Option<NonNull<task::Task>> = None;

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

const INITIAL_FPSCR: u32 = 0;

pub fn set_task_table(tasks: &mut [task::Task]) {
    let prev_task_table = unsafe {
        core::mem::replace(
            &mut TASK_TABLE_BASE,
            Some(NonNull::from(&mut tasks[0])),
        )
    };
    // Catch double-uses of this function.
    assert_eq!(prev_task_table, None);
    // Record length as well.
    unsafe {
        TASK_TABLE_SIZE = tasks.len();
    }
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

#[allow(non_snake_case)]
#[naked]
#[no_mangle]
pub unsafe fn SVCall() {
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

/// Rust-side syscall handler, phase one. This is responsible for doing unsafe
/// environment setup before calling the safe handler.
///
/// The arguments are prepared by the asm ISR as a side effect of its work, so
/// we can avoid recomputing them. This is arguably a premature optimization.
#[no_mangle]
unsafe extern "C" fn syscall_entry(nr: u32, task: *mut task::Task) {
    // Manufacture an exclusive reference to the task table. We can do this
    // "safely" because of the constraints on how we're called, above.
    let tasks = core::slice::from_raw_parts_mut(
        TASK_TABLE_BASE.unwrap().as_mut(),
        TASK_TABLE_SIZE,
    );
    debug_assert!(task as usize >= tasks.as_ptr() as usize);
    debug_assert!((task as usize) < tasks.as_ptr().offset(TASK_TABLE_SIZE as isize) as usize);
    // Use the task pointer, which now aliases a `&mut` slice and shall not be
    // dereferenced, into a task index. Yeah, we could store the task index
    // alongside the pointer. Maybe later.
    let idx = (task as usize - tasks.as_ptr() as usize)
        / core::mem::size_of::<task::Task>();

    let resched = safe_syscall_entry(nr, idx, tasks);

    match resched {
        task::NextTask::Same => (),
        task::NextTask::Specific(i) => {
            CURRENT_TASK_PTR = Some(NonNull::from(&mut tasks[i]));
        }
        task::NextTask::Other => {
            let next = crate::task::select(idx, tasks);
            CURRENT_TASK_PTR = Some(NonNull::from(&mut tasks[next]));
        }
    }
}

fn safe_syscall_entry(
    nr: u32,
    task_index: usize,
    tasks: &mut [task::Task],
) -> task::NextTask {
    // Task state consistency check in debug. TODO: probably just remove me
    debug_assert_eq!(tasks[task_index].state,
        task::TaskState::Healthy(task::SchedState::Runnable));

    match nr {
        0 => crate::send(tasks, task_index),
        1 => crate::recv(tasks, task_index),
        2 => crate::reply(tasks, task_index),
        _ => {
            // Bogus syscall number!
            tasks[task_index].force_fault(task::FaultInfo::SyscallUsage(
                task::UsageError::BadSyscallNumber,
            ))
        }
    }
}
