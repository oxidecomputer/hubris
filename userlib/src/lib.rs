#![no_std]
#![feature(llvm_asm)]

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
                attributes: abi::LeaseAttributes::WRITE,
                base_address: x.as_ptr() as u32,
                length: x.len() as u32,
            },
            _marker: PhantomData,
        }
    }
}

#[repr(u32)]
enum Sysnum {
    Send = 0,
    Recv = 1,
    Reply = 2,
    Timer = 3,
    BorrowRead = 4,
    BorrowWrite = 5,
    BorrowInfo = 6,
    IrqControl = 7,
    Panic = 8,
}

pub fn sys_send(
    target: TaskId,
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &[Lease<'_>],
) -> (u32, usize) {
    let mut response_code: u32;
    let mut response_len: usize;
    unsafe {
        llvm_asm! {
            "svc #0"
            : "={r4}"(response_code),
              "={r5}"(response_len)
            : "{r4}"(u32::from(target.0) << 16 | u32::from(operation)),
              "{r5}"(outgoing.as_ptr()),
              "{r6}"(outgoing.len()),
              "{r7}"(incoming.as_mut_ptr()),
              "{r8}"(incoming.len()),
              "{r9}"(leases.as_ptr()),
              "{r10}"(leases.len()),
              "{r11}"(Sysnum::Send)
            : "memory" // TODO probably too conservative?
            : "volatile"
        }
    }
    (response_code, response_len)
}

pub fn sys_recv(buffer: &mut [u8], notification_mask: u32) -> RecvMessage {
    let mut sender: u32;
    let mut operation: u32;
    let mut message_len: usize;
    let mut response_capacity: usize;
    let mut lease_count: usize;

    unsafe {
        llvm_asm! {
            "svc #0"
            : "={r5}"(sender),
              "={r6}"(operation),
              "={r7}"(message_len),
              "={r8}"(response_capacity),
              "={r9}"(lease_count)
            : "{r4}"(buffer.as_mut_ptr()),
              "{r5}"(buffer.len()),
              "{r6}"(notification_mask),
              "{r11}"(Sysnum::Recv)
            : "r4", "memory"  // TODO probably too conservative?
            : "volatile"
        }
    }

    RecvMessage {
        sender: TaskId(sender as u16),
        operation,
        message_len,
        response_capacity,
        lease_count,
    }
}

pub struct RecvMessage {
    pub sender: TaskId,
    pub operation: u32,
    pub message_len: usize,
    pub response_capacity: usize,
    pub lease_count: usize,
}

pub fn sys_reply(peer: TaskId, code: u32, message: &[u8]) {
    unsafe {
        llvm_asm! {
            "svc #0"
            :
            : "{r4}"(peer.0 as u32),
              "{r5}"(code),
              "{r6}"(message.as_ptr()),
              "{r7}"(message.len()),
              "{r11}"(Sysnum::Reply)
            : "r4", "r5" // reserved
            : "volatile"
        }
    }
}

pub fn sys_set_timer(deadline: Option<u64>, notifications: u32) {
    let raw_deadline = deadline.unwrap_or(0);
    unsafe {
        llvm_asm! {
            "svc #0"
            :
            : "{r4}"(deadline.is_some() as u32),
              "{r5}"(raw_deadline as u32),
              "{r6}"((raw_deadline >> 32) as u32),
              "{r7}"(notifications),
              "{r11}"(Sysnum::Timer)
            :
            : "volatile"
        }
    }
}

pub fn sys_borrow_read(
    lender: TaskId,
    index: usize,
    offset: usize,
    dest: &mut [u8],
) -> (u32, usize) {
    let mut rc: u32;
    let mut length: usize;
    unsafe {
        llvm_asm! {
            "svc #0"
            : "={r4}"(rc),
              "={r5}"(length)
            : "{r4}"(lender.0 as u32),
              "{r5}"(index as u32),
              "{r6}"(offset as u32),
              "{r7}"(dest.as_mut_ptr()),
              "{r8}"(dest.len()),
              "{r11}"(Sysnum::BorrowRead)
            : "memory"
            : "volatile"
        }
    }
    (rc, length)
}

pub fn sys_borrow_write(
    lender: TaskId,
    index: usize,
    offset: usize,
    dest: &[u8],
) -> (u32, usize) {
    let mut rc: u32;
    let mut length: usize;
    unsafe {
        llvm_asm! {
            "svc #0"
            : "={r4}"(rc),
              "={r5}"(length)
            : "{r4}"(lender.0 as u32),
              "{r5}"(index as u32),
              "{r6}"(offset as u32),
              "{r7}"(dest.as_ptr()),
              "{r8}"(dest.len()),
              "{r11}"(Sysnum::BorrowWrite)
            : "memory"
            : "volatile"
        }
    }
    (rc, length)
}

pub fn sys_borrow_info(lender: TaskId, index: usize) -> (u32, u32, usize) {
    let mut rc: u32;
    let mut atts: u32;
    let mut length: usize;
    unsafe {
        llvm_asm! {
            "svc #0"
            : "={r4}"(rc),
              "={r5}"(atts),
              "={r6}"(length)
            : "{r4}"(lender.0 as u32),
              "{r5}"(index as u32),
              "{r11}"(Sysnum::BorrowInfo)
            :
            : "volatile"
        }
    }
    (rc, atts, length)
}

pub fn sys_irq_control(mask: u32, enable: bool) {
    unsafe {
        llvm_asm! {
            "svc #0"
            :
            : "{r4}"(mask),
              "{r5}"(enable as u32),
              "{r11}"(Sysnum::IrqControl)
            : "r4", "r5"
            : "volatile"
        }
    }
}

pub fn sys_panic(msg: &[u8]) -> ! {
    unsafe {
        llvm_asm! {
            "svc #0
             udf #0xad"
            :
            : "{r4}"(msg.as_ptr()),
              "{r5}"(msg.len()),
              "{r11}"(Sysnum::Panic)
            :
            : "volatile"
        }
        core::hint::unreachable_unchecked()
    }
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
