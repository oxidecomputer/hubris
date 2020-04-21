#![no_std]
#![no_main]
#![feature(asm)]

// pick a panicking behavior
// extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics
// extern crate panic_abort; // requires nightly
// extern crate panic_itm; // logs messages over ITM; requires ITM support
extern crate panic_semihosting; // logs messages to the host stderr; requires a debugger
extern crate stm32f4;

use cortex_m_rt::entry;

use kern::app::{App, RegionAttributes, RegionDesc, TaskDesc, TaskFlags};

#[repr(C)]
struct Descriptors {
    app: App,
    task: [TaskDesc; 2],
    region: RegionDesc,
}

static mut KERNEL_RAM: [u8; 1024] = [0; 1024];

#[entry]
fn main() -> ! {
    let app: Descriptors = Descriptors {
        app: App {
            magic: kern::app::CURRENT_APP_MAGIC,
            region_count: 1,
            task_count: 2,
            zeroed_expansion_space: [0; 20],
        },
        task: [
            TaskDesc {
                entry_point: sender as usize as u32,
                flags: TaskFlags::START_AT_BOOT,
                initial_stack: 0x10010000,
                priority: kern::app::Priority(1),
                regions: [0, 0, 0, 0, 0, 0, 0, 0],
            },
            TaskDesc {
                entry_point: rxer as usize as u32,
                flags: TaskFlags::START_AT_BOOT,
                initial_stack: 0x10008000,
                priority: kern::app::Priority(0),
                regions: [0, 0, 0, 0, 0, 0, 0, 0],
            },
        ],
        region: RegionDesc {
            base: 0,
            size: !0,
            attributes: RegionAttributes::RWX,
            reserved_zero: 0,
        },
    };

    unsafe {
        kern::startup::start_kernel(
            &app.app,
            KERNEL_RAM.as_mut_ptr(),
            KERNEL_RAM.len(),
        )
    }
}

/// Loops sending an empty message.
fn sender() -> ! {
    loop {
        unsafe {
            asm! {
                "svc #0"
                :
                : "{r4}"(1 << 16),
                  "{r5}"(0),
                  "{r6}"(0),
                  "{r7}"(0),
                  "{r8}"(0),
                  "{r9}"(0),
                  "{r10}"(0),
                  "{r11}"(0)
                :
                : "volatile"
            }
        }
    }
}

/// Loops receiving and responding to messages.
fn rxer() -> ! {
    let mut rx_buf = [0u8; 0];
    loop {
        // Receive message
        let mut sender: u32;
        #[allow(unused_variables)]
        let mut operation: u32;
        #[allow(unused_variables)]
        let mut message_len: usize;
        #[allow(unused_variables)]
        let mut response_capacity: usize;
        #[allow(unused_variables)]
        let mut lease_count: usize;

        #[allow(unused_assignments)]
        unsafe {
            asm! {
                "svc #0"
                : "={r4}"(sender),
                  "={r5}"(operation),
                  "={r6}"(message_len),
                  "={r7}"(response_capacity),
                  "={r8}"(lease_count)
                : "{r4}"(rx_buf.as_mut_ptr())
                  "{r5}"(rx_buf.len())
                  "{r11}"(1)
                :
                : "volatile"
            }
        }
        // Unblock sender
        let response_code: u32 = 0;
        unsafe {
            asm! {
                "svc #0"
                :
                : "{r4}"(sender)
                  "{r5}"(response_code)
                  "{r6}"(rx_buf.as_mut_ptr())
                  "{r7}"(rx_buf.len())
                  "{r11}"(2)
                :
                : "volatile"
            }
        }
    }
}
