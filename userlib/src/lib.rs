//! User application support library for Hubris.
//!
//! This contains syscall stubs and types, and re-exports the contents of the
//! `abi` crate that gets shared with the kernel.
//!
//! # Syscall stub implementations
//!
//! Each syscall stub consists of two parts: a public `sys_foo` function
//! intended for use by programs, and an internal `sys_foo_stub` function. This
//! might seem like needless duplication, and in a way, it is.
//!
//! Limitations in the behavior of the current `asm!` feature mean we have a
//! hard time moving values into registers r6, r7, and r11. Because (for better
//! or worse) the syscall ABI uses these registers, we have to take extra steps.
//!
//! The `stub` function contains the actual `asm!` call sequence. It is `naked`,
//! meaning the compiler will *not* attempt to do any framepointer/basepointer
//! nonsense, and we can thus reason about the assignment and availability of
//! all registers. It's `inline(never)` because, were it to be inlined, that
//! assumption would be violated.
//!
//! See: https://github.com/rust-lang/rust/issues/73450#issuecomment-650463347

#![no_std]
#![feature(asm)]
#![feature(naked_functions)]

pub use abi::*;
pub use num_derive::{FromPrimitive, ToPrimitive};
pub use num_traits::{FromPrimitive, ToPrimitive};

use core::marker::PhantomData;

pub mod hl;
pub mod kipc;

#[derive(Debug)]
#[repr(transparent)]
pub struct Lease<'a> {
    kern_rep: abi::ULease,
    _marker: PhantomData<&'a mut ()>,
}

impl<'a> From<&'a [u8]> for Lease<'a> {
    fn from(x: &'a [u8]) -> Self {
        Self {
            kern_rep: abi::ULease {
                attributes: abi::LeaseAttributes::READ,
                base_address: x.as_ptr() as u32,
                length: x.len() as u32,
            },
            _marker: PhantomData,
        }
    }
}

impl<'a> From<&'a mut [u8]> for Lease<'a> {
    fn from(x: &'a mut [u8]) -> Self {
        Self {
            kern_rep: abi::ULease {
                attributes: LeaseAttributes::READ | LeaseAttributes::WRITE,
                base_address: x.as_ptr() as u32,
                length: x.len() as u32,
            },
            _marker: PhantomData,
        }
    }
}

/// Return type for stubs that return an `(rc, len)` tuple, because the layout
/// of tuples is not specified in the C ABI, and we're using the C ABI to
/// interface to assembler.
///
/// Register-return of structs is also not guaranteed by the C ABI, so we
/// represent the pair of returned registers with something that *can* get
/// passed back in registers: a `u64`.
#[repr(transparent)]
struct RcLen(u64);

impl From<RcLen> for (u32, usize) {
    fn from(s: RcLen) -> Self {
        (s.0 as u32, (s.0 >> 32) as usize)
    }
}

#[inline(always)]
pub fn sys_send(
    target: TaskId,
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &[Lease<'_>],
) -> (u32, usize) {
    let mut args = SendArgs {
        packed_target_operation: u32::from(target.0) << 16 | u32::from(operation),
        outgoing_ptr: outgoing.as_ptr(),
        outgoing_len: outgoing.len(),
        incoming_ptr: incoming.as_mut_ptr(),
        incoming_len: incoming.len(),
        lease_ptr: leases.as_ptr(),
        lease_len: leases.len(),
    };
    unsafe {
        sys_send_stub(&mut args).into()
    }
}

#[allow(dead_code)] // this gets used from asm
#[repr(C)] // field order matters
struct SendArgs<'a> {
    packed_target_operation: u32,
    outgoing_ptr: *const u8,
    outgoing_len: usize,
    incoming_ptr: *mut u8,
    incoming_len: usize,
    lease_ptr: *const Lease<'a>,
    lease_len: usize,
}

/// Core implementation of the SEND syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_send_stub(_args: &mut SendArgs<'_>) -> RcLen {
    asm!("
        @ Spill the registers we're about to use to pass stuff.
        push {{r4-r11}}
        @ Load in args from the struct.
        ldm r0, {{r4-r10}}
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ Move the two results back into their return positions.
        mov r0, r4
        mov r1, r5
        @ Restore the registers we used.
        pop {{r4-r11}}
        @ Fin.
        bx lr
        ",
        sysnum = const Sysnum::Send as u32,
        options(noreturn),
    )
}

#[inline(always)]
pub fn sys_recv(buffer: &mut [u8], notification_mask: u32) -> RecvMessage {
    use core::mem::MaybeUninit;

    let mut out = MaybeUninit::<RawRecvMessage>::uninit();
    unsafe {
        sys_recv_stub(
            buffer.as_mut_ptr(),
            buffer.len(),
            notification_mask,
            out.as_mut_ptr(),
        );
    }
    // Safety: stub fully initializes output struct.
    let out = unsafe { out.assume_init() };

    RecvMessage {
        sender: TaskId(out.sender as u16),
        operation: out.operation,
        message_len: out.message_len,
        response_capacity: out.response_capacity,
        lease_count: out.lease_count,
    }
}

pub struct RecvMessage {
    pub sender: TaskId,
    pub operation: u32,
    pub message_len: usize,
    pub response_capacity: usize,
    pub lease_count: usize,
}

/// Core implementation of the RECV syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_recv_stub(
    _buffer_ptr: *mut u8,
    _buffer_len: usize,
    _notification_mask: u32,
    _out: *mut RawRecvMessage,
) {
    asm!("
        @ Spill the registers we're about to use to pass stuff.
        push {{r4-r11}}
        @ Move register arguments into their proper positions.
        mov r4, r0
        mov r5, r1
        mov r6, r2
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ Write all the results out into the raw output buffer.
        stm r3, {{r5-r9}}
        @ Restore the registers we used.
        pop {{r4-r11}}
        @ Fin.
        bx lr
        ",
        sysnum = const Sysnum::Recv as u32,
        options(noreturn),
    )
}

/// Duplicated version of `RecvMessage` with all 32-bit fields and predictable
/// field order, so that it can be generated from assembly.
///
/// TODO: might be able to merge this into actual `RecvMessage` with some care.
#[repr(C)]
struct RawRecvMessage {
    pub sender: u32,
    pub operation: u32,
    pub message_len: usize,
    pub response_capacity: usize,
    pub lease_count: usize,
}

#[inline(always)]
pub fn sys_reply(peer: TaskId, code: u32, message: &[u8]) {
    unsafe {
        sys_reply_stub(
            peer.0 as u32,
            code,
            message.as_ptr(),
            message.len()
        )
    }
}

/// Core implementation of the REPLY syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_reply_stub(
    _peer: u32,
    _code: u32,
    _message_ptr: *const u8,
    _message_len: usize,
) {
    asm!("
        @ Spill the registers we're about to use to pass stuff. Note that we're
        @ being clever and pushing only the registers we need; this means the
        @ pop sequence at the end needs to match! (Why are we pushing LR? Because
        @ the ABI requires us to maintain 8-byte stack alignment, so we must
        @ push registers in pairs.)
        push {{r4-r7, r11, lr}}

        @ Move register arguments into place.
        mov r4, r0
        mov r5, r1
        mov r6, r2
        mov r7, r3
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ This call has no results.

        @ Restore the registers we used and return.
        pop {{r4-r7, r11, pc}}
        ",
        sysnum = const Sysnum::Reply as u32,
        options(noreturn),
    )
}

/// Sets this task's timer.
///
/// The timer is set to `deadline`. If `deadline` is `None`, the timer is
/// disabled. Otherwise, the timer is configured to notify when the specified
/// time (in ticks since boot) is reached. When that occurs, the `notifications`
/// will get posted to this task, and the timer will be disabled.
///
/// If the deadline is chosen such that the timer *would have already fired*,
/// had it been set earlier -- that is, if the deadline is `<=` the current time
/// -- the `notifications` will be posted immediately and the timer will not be
/// enabled.
#[inline(always)]
pub fn sys_set_timer(deadline: Option<u64>, notifications: u32) {
    let raw_deadline = deadline.unwrap_or(0);
    unsafe {
        sys_set_timer_stub(
            deadline.is_some() as u32,
            raw_deadline as u32,
            (raw_deadline >> 32) as u32,
            notifications,
        )
    }
}

/// Core implementation of the SET_TIMER syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_set_timer_stub(
    _set_timer: u32,
    _deadline_lo: u32,
    _deadline_hi: u32,
    _notification: u32,
) {
    asm!("
        @ Spill the registers we're about to use to pass stuff. Note that we're
        @ being clever and pushing only the registers we need; this means the
        @ pop sequence at the end needs to match! (Why are we pushing LR? Because
        @ the ABI requires us to maintain 8-byte stack alignment, so we must
        @ push registers in pairs.)
        push {{r4-r7, r11, lr}}

        @ Move register arguments into place.
        mov r4, r0
        mov r5, r1
        mov r6, r2
        mov r7, r3
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ This call has no results.

        @ Restore the registers we used and return.
        pop {{r4-r7, r11, pc}}
        ",
        sysnum = const Sysnum::SetTimer as u32,
        options(noreturn),
    )
}

#[inline(always)]
pub fn sys_borrow_read(
    lender: TaskId,
    index: usize,
    offset: usize,
    dest: &mut [u8],
) -> (u32, usize) {
    let mut args = BorrowReadArgs {
        lender: lender.0 as u32,
        index,
        offset,
        dest: dest.as_mut_ptr(),
        dest_len: dest.len(),
    };
    unsafe {
        sys_borrow_read_stub(&mut args).into()
    }
}

/// Core implementation of the BORROW_READ syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_borrow_read_stub(
    _args: *mut BorrowReadArgs,
) -> RcLen {
    asm!("
        @ Spill the registers we're about to use to pass stuff. Note that we're
        @ being clever and pushing only the registers we need; this means the
        @ pop sequence at the end needs to match!
        push {{r4-r8, r11}}

        @ Move register arguments into place.
        ldm r0, {{r4-r8}}
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ Move the results into place.
        mov r0, r4
        mov r1, r5

        @ Restore the registers we used and return.
        pop {{r4-r8, r11}}
        bx lr
        ",
        sysnum = const Sysnum::BorrowRead as u32,
        options(noreturn),
    )
}

#[repr(C)]
struct BorrowReadArgs {
    lender: u32,
    index: usize,
    offset: usize,
    dest: *mut u8,
    dest_len: usize,
}

#[inline(always)]
pub fn sys_borrow_write(
    lender: TaskId,
    index: usize,
    offset: usize,
    src: &[u8],
) -> (u32, usize) {
    let mut args = BorrowWriteArgs {
        lender: lender.0 as u32,
        index,
        offset,
        src: src.as_ptr(),
        src_len: src.len(),
    };
    unsafe {
        sys_borrow_write_stub(&mut args).into()
    }
}

/// Core implementation of the BORROW_WRITE syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_borrow_write_stub(
    _args: *mut BorrowWriteArgs,
) -> RcLen {
    asm!("
        @ Spill the registers we're about to use to pass stuff. Note that we're
        @ being clever and pushing only the registers we need; this means the
        @ pop sequence at the end needs to match!
        push {{r4-r8, r11}}

        @ Move register arguments into place.
        ldm r0, {{r4-r8}}
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ Move the results into place.
        mov r0, r4
        mov r1, r5

        @ Restore the registers we used and return.
        pop {{r4-r8, r11}}
        bx lr
        ",
        sysnum = const Sysnum::BorrowWrite as u32,
        options(noreturn),
    )
}

#[repr(C)]
struct BorrowWriteArgs {
    lender: u32,
    index: usize,
    offset: usize,
    src: *const u8,
    src_len: usize,
}

#[inline(always)]
pub fn sys_borrow_info(lender: TaskId, index: usize) -> (u32, u32, usize) {
    use core::mem::MaybeUninit;

    let mut raw = MaybeUninit::<RawBorrowInfo>::uninit();
    unsafe {
        sys_borrow_info_stub(
            lender.0 as u32,
            index,
            raw.as_mut_ptr(),
        );
    }
    // Safety: stub completely initializes record
    let raw = unsafe { raw.assume_init() };

    (raw.rc, raw.atts, raw.length)
}

#[repr(C)]
struct RawBorrowInfo {
    rc: u32,
    atts: u32,
    length: usize,
}

/// Core implementation of the BORROW_INFO syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_borrow_info_stub(
    _lender: u32,
    _index: usize,
    _out: *mut RawBorrowInfo,
) {
    asm!("
        @ Spill the registers we're about to use to pass stuff. Note that we're
        @ being clever and pushing only the registers we need; this means the
        @ pop sequence at the end needs to match!
        push {{r4-r6, r11}}

        @ Move register arguments into place.
        mov r4, r0
        mov r5, r1
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ Move the results into place.
        stm r2, {{r4-r6}}

        @ Restore the registers we used and return.
        pop {{r4-r6, r11}}
        bx lr
        ",
        sysnum = const Sysnum::BorrowInfo as u32,
        options(noreturn),
    )
}

#[inline(always)]
pub fn sys_irq_control(mask: u32, enable: bool) {
    unsafe {
        sys_irq_control_stub(mask, enable as u32);
    }
}

/// Core implementation of the IRQ_CONTROL syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_irq_control_stub(
    _mask: u32,
    _enable: u32,
) {
    asm!("
        @ Spill the registers we're about to use to pass stuff. Note that we're
        @ being clever and pushing only the registers we need; this means the
        @ pop sequence at the end needs to match!
        push {{r4, r5, r11, lr}}

        @ Move register arguments into place.
        mov r4, r0
        mov r5, r1
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ This call returns no results.

        @ Restore the registers we used and return.
        pop {{r4, r5, r11, pc}}
        ",
        sysnum = const Sysnum::IrqControl as u32,
        options(noreturn),
    )
}

#[inline(always)]
pub fn sys_panic(msg: &[u8]) -> ! {
    unsafe {
        sys_panic_stub(msg.as_ptr(), msg.len())
    }
}

/// Core implementation of the PANIC syscall.
///
/// See the note on syscall stubs at the top of this module for rationale.
#[inline(never)]
#[naked]
unsafe extern "C" fn sys_panic_stub(
    _msg: *const u8,
    _len: usize,
) -> ! {
    asm!("
        @ We're not going to return, so technically speaking we don't need to
        @ save registers. However, we save them anyway, so that we can reconstruct
        @ the state that led to the panic.
        push {{r4, r5, r11, lr}}

        @ Move register arguments into place.
        mov r4, r0
        mov r5, r1
        @ Load the constant syscall number.
        mov r11, {sysnum}

        @ To the kernel!
        svc #0

        @ This really shouldn't return. Ensure this:
        udf #0xad
        ",
        sysnum = const Sysnum::Panic as u32,
        options(noreturn),
    )
}

#[cfg(feature = "log-itm")]
#[macro_export]
macro_rules! sys_log {
    ($s:expr) => {
        unsafe {
            let stim = &mut (*cortex_m::peripheral::ITM::ptr()).stim[1];
            cortex_m::iprintln!(stim, $s);
        }
    };
    ($s:expr, $($tt:tt)*) => {
        unsafe {
            let stim = &mut (*cortex_m::peripheral::ITM::ptr()).stim[1];
            cortex_m::iprintln!(stim, $s, $($tt)*);
        }
    };
}

#[cfg(feature = "log-semihosting")]
#[macro_export]
macro_rules! sys_log {
    ($s:expr) => {
        let _ = cortex_m_semihosting::hprintln!($s);
    };
    ($s:expr, $($tt:tt)*) => {
        let _ = cortex_m_semihosting::hprintln!($s, $($tt)*);
    };
}

#[cfg(not(any(feature = "log-semihosting", feature = "log-itm")))]
#[macro_export]
macro_rules! sys_log {
    ($s:expr) => {
        compile_error!(concat!(
            "to use sys_log! must enable either ",
            "'log-semihosting' or 'log-itm' feature"
        ))
    };
    ($s:expr, $($tt:tt)*) => {
        compile_error!(concat!(
            "to use sys_log! must enable either ",
            "'log-semihosting' or 'log-itm' feature"
        ))
    };
}

/// This is the entry point for the kernel. Its job is to set up our memory
/// before jumping to user-defined `main`.
#[doc(hidden)]
#[no_mangle]
#[link_section = ".text.start"]
pub unsafe extern "C" fn _start() -> ! {
    // Symbols from the linker script:
    extern "C" {
        static mut __sbss: u32;
        static mut __ebss: u32;
        static mut __sdata: u32;
        static mut __edata: u32;
        static __sidata: u32;
    }

    // Provided by the user program:
    extern "Rust" {
        fn main() -> !;
    }

    // Initialize RAM
    r0::zero_bss(&mut __sbss, &mut __ebss);
    r0::init_data(&mut __sdata, &mut __edata, &__sidata);

    // Do *not* reorder any instructions from main above this point.
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);

    main()
}

#[cfg(feature = "panic-messages")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;

    // Burn some stack to try to get at least the prefix of the panic info
    // recorded.
    struct PrefixWrite([u8; 128], usize);

    impl Write for PrefixWrite {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let space_left = self.0.len() - self.1;
            let n = space_left.min(s.len());
            if n != 0 {
                self.0[self.1..self.1 + n].copy_from_slice(&s.as_bytes()[..n]);
                self.1 += n;
            }
            Ok(())
        }
    }

    let mut pw = PrefixWrite([0; 128], 0);
    write!(pw, "{}", info).ok();
    sys_panic(&pw.0[..pw.1])
}

#[cfg(not(feature = "panic-messages"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    sys_panic(b"PANIC")
}

// Enumeration of tasks in the application, for convenient reference, generated
// by build.rs.
//
// The `Task` enum will contain one entry per task defined in the application,
// with the value of that task's index. The `SELF` constant refers to the
// current task. e.g.
//
// ```
// enum Task {
//     Init = 0,
//     Foo = 1,
//     Bar = 2,
// }
//
// pub const SELF: Task = Task::Foo;
// ```
//
// When building a single task outside the context of an application, there will
// be exactly one "task" in the enum, called `anonymous`.
include!(concat!(env!("OUT_DIR"), "/tasks.rs"));
