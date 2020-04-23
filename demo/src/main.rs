#![no_std]
#![no_main]
#![feature(asm)]

#[cfg(not(any(feature = "panic-halt", feature = "panic-semihosting")))]
compile_error!(
    "Must have either feature panic-halt or panic-semihosting enabled"
);

// Panic behavior controlled by Cargo features:
#[cfg(feature = "panic-halt")]
extern crate panic_halt; // breakpoint on `rust_begin_unwind` to catch panics
#[cfg(feature = "panic-semihosting")]
extern crate panic_semihosting; // requires a debugger

use cortex_m_rt::entry;
use stm32f4::stm32f407 as device;

use kern::app::{App, RegionAttributes, RegionDesc, TaskDesc, TaskFlags};

#[repr(C)]
struct Descriptors {
    app: App,
    task: [TaskDesc; 2],
    region: [RegionDesc; 6],
}

static mut KERNEL_RAM: [u8; 1024] = [0; 1024];

// These values MUST be synchronized with task linker scripts.
const PING_ENTRY_POINT: u32 = 0x0802_0000;
const PONG_ENTRY_POINT: u32 = 0x0802_4000;

const PING_RAM: u32 = 0x1000_0000;
const PING_INITIAL_STACK: u32 = PING_RAM + 0x400;
const PONG_RAM: u32 = 0x1000_0400;
const PONG_INITIAL_STACK: u32 = PONG_RAM + 0x400;

#[no_mangle]
#[link_section = ".task_ping_image"]
pub static PING_IMAGE: [u8; 16384] =
    include!(env!("TASK_PING_PATH"));

#[no_mangle]
#[link_section = ".task_pong_image"]
pub static PONG_IMAGE: [u8; 16384] =
    include!(env!("TASK_PONG_PATH"));

#[entry]
fn main() -> ! {
    let p = device::Peripherals::take().unwrap();
    // Turn on clock to GPIOD.
    p.RCC.ahb1enr.modify(|_, w| {
        w.gpioden().enabled()
    });
    // Make pin D12 and D13 outputs.
    p.GPIOD.moder.modify(|_, w| {
        w.moder12().output().moder13().output()
    });

    let app: Descriptors = Descriptors {
        app: App {
            magic: kern::app::CURRENT_APP_MAGIC,
            region_count: 6,
            task_count: 2,
            zeroed_expansion_space: [0; 20],
        },
        task: [
            // ping
            TaskDesc {
                entry_point: PING_ENTRY_POINT,
                flags: TaskFlags::START_AT_BOOT,
                initial_stack: PING_INITIAL_STACK,
                priority: kern::app::Priority(1),
                regions: [1, 2, 5, 0, 0, 0, 0, 0],
            },
            TaskDesc {
                entry_point: PONG_ENTRY_POINT,
                flags: TaskFlags::START_AT_BOOT,
                initial_stack: PONG_INITIAL_STACK,
                priority: kern::app::Priority(0),
                regions: [3, 4, 5, 0, 0, 0, 0, 0],
            },
        ],
        region: [
            // A "null" region giving no authority, to fill out region tables.
            RegionDesc {
                base: 0,
                size: 32,
                attributes: RegionAttributes::empty(),
                reserved_zero: 0,
            },
            // Ping flash
            RegionDesc {
                base: PING_ENTRY_POINT,
                size: 0x4000,
                attributes: RegionAttributes::READ | RegionAttributes::EXECUTE,
                reserved_zero: 0,
            },
            // Ping RAM
            RegionDesc {
                base: PING_RAM,
                size: 0x400,
                attributes: RegionAttributes::READ | RegionAttributes::WRITE,
                reserved_zero: 0,
            },
            // Pong flash
            RegionDesc {
                base: PONG_ENTRY_POINT,
                size: 0x4000,
                attributes: RegionAttributes::READ | RegionAttributes::EXECUTE,
                reserved_zero: 0,
            },
            // Pong RAM
            RegionDesc {
                base: PONG_RAM,
                size: 0x400,
                attributes: RegionAttributes::READ | RegionAttributes::WRITE,
                reserved_zero: 0,
            },
            // GPIOD
            RegionDesc {
                base: 0x4002_0C00,
                size: 0x400,
                attributes: RegionAttributes::READ | RegionAttributes::WRITE | RegionAttributes::DEVICE,
                reserved_zero: 0,
            },
        ],
    };

    unsafe {
        kern::startup::start_kernel(
            &app.app,
            KERNEL_RAM.as_mut_ptr(),
            KERNEL_RAM.len(),
        )
    }
}
