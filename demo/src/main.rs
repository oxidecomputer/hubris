#![no_std]
#![no_main]

#![feature(const_raw_ptr_to_usize_cast)] // FIXME VERY TEMPORARY

// pick a panicking behavior
// extern crate panic_halt; // you can put a breakpoint on `rust_begin_unwind` to catch panics
// extern crate panic_abort; // requires nightly
// extern crate panic_itm; // logs messages over ITM; requires ITM support
extern crate panic_semihosting; // logs messages to the host stderr; requires a debugger
extern crate stm32f4;

use cortex_m_rt::entry;

use kern::app::{App, RegionDesc, TaskDesc, RegionAttributes, TaskFlags};

#[repr(C)]
struct Descriptors {
    app: App,
    task: TaskDesc,
    region: RegionDesc,
}

static APP: Descriptors = Descriptors {
    app: App {
        magic: kern::app::CURRENT_APP_MAGIC,
        region_count: 1,
        task_count: 1,
        zeroed_expansion_space: [0; 20],
    },
    task: TaskDesc {
        entry_point: 0,
        flags: TaskFlags::START_AT_BOOT,
        initial_stack: 0x10010000,
        priority: kern::task::Priority(0),
        regions: [0, 0, 0, 0, 0, 0, 0, 0],
    },
    region: RegionDesc {
        base: 0,
        size: !0,
        attributes: RegionAttributes::RWX,
        reserved_zero: 0,
    },
};

static mut KERNEL_RAM: [u8; 1024] = [0; 1024];

#[entry]
fn main() -> ! {
    unsafe {
        kern::startup::start_kernel(
            &APP.app,
            KERNEL_RAM.as_mut_ptr(),
            KERNEL_RAM.len(),
        )
    }
}

fn spin() -> !{
    loop {
        cortex_m::asm::nop();
    }
}
