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

use kern::app::App;

extern "C" {
    static hubris_app_table: App;
    static mut __sheap: u8;
    static __eheap: u8;
}

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

    unsafe {
        let heap_size = (&__eheap as *const _ as usize)
            - (&__sheap as *const _ as usize);
        kern::startup::start_kernel(
            &hubris_app_table,
            (&mut __sheap) as *mut _,
            heap_size,
        )
    }
}
