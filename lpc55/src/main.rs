#![no_std]
#![no_main]
#![feature(llvm_asm)]

#[cfg(not(any(feature = "panic-itm", feature = "panic-semihosting")))]
compile_error!(
    "Must have either feature panic-itm or panic-semihosting enabled"
);

// Panic behavior controlled by Cargo features:
#[cfg(feature = "panic-itm")]
extern crate panic_itm; // breakpoint on `rust_begin_unwind` to catch panics
#[cfg(feature = "panic-semihosting")]
extern crate panic_semihosting; // requires a debugger

use cortex_m_rt::entry;
use kern::app::App;
use lpc55_pac as device;

extern "C" {
    static hubris_app_table: App;
    static mut __sheap: u8;
    static __eheap: u8;
}

#[entry]
fn main() -> ! {

    unsafe {
        //
        // To allow for SWO (the vector for ITM output), we must explicitly
        // enable it on pin0_10.
        //
        let iocon = &*device::IOCON::ptr();
        iocon.pio0_10.modify(|_, w| w.func().alt6());

        let heap_size = (&__eheap as *const _ as usize)
            - (&__sheap as *const _ as usize);
        kern::startup::start_kernel(
            &hubris_app_table,
            (&mut __sheap) as *mut _,
            heap_size,
        )
    }
}
