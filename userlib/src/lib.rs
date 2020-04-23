#![no_std]
#![feature(asm)]

use core::marker::PhantomData;

#[derive(Debug)]
#[repr(transparent)]
pub struct Lease<'a> {
    kern_rep: abi::ULease,
    _marker: PhantomData<&'a mut ()>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct TaskId(pub u16);

impl TaskId {
    pub const KERNEL: Self = Self(0xFFFF);

    pub fn for_index_and_gen(index: usize, gen: usize) -> Self {
        assert!(index < 0x1000);
        assert!(gen < 0x10);
        TaskId((index as u16 & 0xFFF) | (gen as u16) << 12)
    }
}

#[repr(u32)]
enum Sysnum {
    Send = 0,
    Recv = 1,
    Reply = 2,
    Timer = 3,
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
        asm! {
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

pub fn sys_recv(
    buffer: &mut [u8],
    notification_mask: u32,
) -> RecvMessage {
    let mut sender: u32;
    let mut operation: u32;
    let mut message_len: usize;
    let mut response_capacity: usize;
    let mut lease_count: usize;

    unsafe {
        asm! {
            "svc #0"
            : "={r4}"(sender),
              "={r5}"(operation),
              "={r6}"(message_len),
              "={r7}"(response_capacity),
              "={r8}"(lease_count)
            : "{r4}"(buffer.as_mut_ptr()),
              "{r5}"(buffer.len()),
              "{r6}"(notification_mask),
              "{r11}"(Sysnum::Recv)
            : "memory"  // TODO probably too conservative?
            : "volatile"
        }
    }

    RecvMessage {
        sender: TaskId(sender as u16),
        operation: operation as u16,
        message_len,
        response_capacity,
        lease_count,
    }
}

pub struct RecvMessage {
    pub sender: TaskId,
    pub operation: u16,
    pub message_len: usize,
    pub response_capacity: usize,
    pub lease_count: usize,
}

pub fn sys_reply(
    peer: TaskId,
    code: u32,
    message: &[u8],
) {
    unsafe {
        asm! {
            "svc #0"
            :
            : "{r4}"(peer.0 as u32),
              "{r5}"(code),
              "{r6}"(message.as_ptr()),
              "{r7}"(message.len()),
              "{r11}"(Sysnum::Reply)
            :
            : "volatile"
        }
    }
}

pub fn sys_set_timer(
    deadline: Option<u64>,
    notifications: u32,
) {
    let raw_deadline = deadline.unwrap_or(0);
    unsafe {
        asm! {
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
