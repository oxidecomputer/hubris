#![no_std]
#![no_main]
#![feature(llvm_asm)]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use userlib::*;
use cortex_m_semihosting::hprintln;
use stm32f4::stm32f407 as device;
use zerocopy::AsBytes;

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const RCC: Task = SELF;

#[export_name = "main"]
fn main() -> ! {
    turn_on_gpiod();
    // pin D4 controls the reset to the chip
    let gpiod = unsafe { &*device::GPIOD::ptr() };

    gpiod.moder.modify(|_, w| {
        w.moder4().output()
    });

    gpiod.bsrr.write(|w| {
        w.bs4().set_bit()
    });

    gpiod.ospeedr.modify(|_, w| {
        w.ospeedr4().medium_speed()
    });

    let addr : &[u8]= &[0x1];
    let i2c = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    hprintln!("Starting to spam!");
    loop {
        let mut recv : [u8; 4] = [0; 4];
        let a : &mut [u8] = &mut recv;
        // We have a slave configured at address 0x4a
        let (code, _) = sys_send(i2c, 1, &[0x4a], &mut [], &[Lease::from(addr)]);
        if code != 0 {
            hprintln!("Got error code{}", code);
        } else {
            hprintln!("Success");
        }
        let (code, _) = sys_send(i2c, 2, &[0x4a], &mut [], &[Lease::from(a)]);
        if code != 0 {
            hprintln!("Got error code{}", code);
        } else {
            hprintln!("Got buffer {:x?}", recv[0]);
        }
    }
}

fn turn_on_gpiod() {
    let rcc_driver = TaskId::for_index_and_gen(RCC as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 3; // see bits in AHB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

