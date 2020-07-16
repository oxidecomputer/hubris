#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
//extern crate userlib;
use lpc55_pac as device;
use drv_lpc55_syscon_api::{Peripheral, Syscon};
use userlib::*;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = SELF;

// This eats up a lot of stack as one might guess from this size
// Just keep it in .bss for now
// The size is 1192 bytes but since everything comes out in u32
// keep it as such
static mut ACTIVATION_CODE : [u32; 298] = [0; 298];

#[export_name = "main"]
fn main() -> ! {
    let syscon = Syscon::from(
        TaskId::for_index_and_gen(SYSCON as usize, Generation::default()));

    let puf = unsafe { &*device::PUF::ptr() };

    // So long term we'll need to figure out where to place the AC after
    // we do the enroll for the first time. For now, we just get a different
    // value each time
    puf_init(puf, &syscon);

    let result = unsafe { puf_enroll(puf, &mut ACTIVATION_CODE) };
    if !result {
        cortex_m_semihosting::hprintln!("enroll fail!");
    }

    turn_off_puf(puf, &syscon);
    puf_init(puf, &syscon);

    let result = unsafe { puf_start(puf, &ACTIVATION_CODE) };
    if !result {
        cortex_m_semihosting::hprintln!("start fail!");
    }

    let mut key_code : [u32; 214] = [0; 214];
    
    let result = puf_set_intrisic_key(puf, 1, 4096, &mut key_code);
    if !result {
        cortex_m_semihosting::hprintln!("set intrinsic fail!");
    }

    cortex_m_semihosting::hprintln!("done!");
    loop { }
}

fn puf_init(puf : &device::puf::RegisterBlock, syscon: &Syscon) {
    turn_on_puf(puf, syscon);
    puf_wait_for_init(puf);
}

fn turn_off_puf(puf : &device::puf::RegisterBlock, syscon: &Syscon) {
    puf.pwrctrl.write(|w| w.ramon().clear_bit());

    // need to wait 400 ms
    // 1 tick = 1 ms
    hl::sleep_for(400);

    syscon.enter_reset(Peripheral::Puf);
    syscon.disable_clock(Peripheral::Puf);
}

fn turn_on_puf(puf : &device::puf::RegisterBlock, syscon: &Syscon) {
    syscon.enable_clock(Peripheral::Puf);

    // The NXP C driver explicitly puts this in reset so do this
    // just to be on the safe side...
    syscon.enter_reset(Peripheral::Puf);
    syscon.leave_reset(Peripheral::Puf);

    puf.pwrctrl.write(|w| w.ramon().set_bit());

    while ! puf.pwrctrl.read().ramstat().bit() { }
}

fn puf_wait_for_init(puf : &device::puf::RegisterBlock) -> bool {
    while puf.stat.read().busy().bit() { }

    if puf.stat.read().success().bit() && ! puf.stat.read().error().bit() {
        return true;
    } else {
        return false;
    }
}

// do you put an upper bound on this? no? idk
fn puf_enroll(puf : &device::puf::RegisterBlock, acdata : &mut [u32; 298]) -> bool {
    let mut idx = 0;

    if ! puf.allow.read().allowenroll().bit() {
        return false;
    }

    // begin Enroll
    puf.ctrl.write(|w| w.enroll().set_bit());

    // wait
    while ! puf.stat.read().busy().bit() && ! puf.stat.read().error().bit() { }

    while puf.stat.read().busy().bit() {
        if puf.stat.read().codeoutavail().bit() {
            let d = puf.codeoutput.read().bits();
            acdata[idx] = d;
            idx += 1;
        }
    }

    return puf.stat.read().success().bit();
}

fn puf_start(puf : &device::puf::RegisterBlock, activation_code: &[u32; 298]) -> bool {
    let mut idx = 0;

    if ! puf.allow.read().allowstart().bit() {
        cortex_m_semihosting::hprintln!("no start!");
        return false;
    }


    puf.ctrl.write(|w| w.start().set_bit());

   
    while ! puf.stat.read().busy().bit() && ! puf.stat.read().error().bit() { }

    while puf.stat.read().busy().bit() {
        if puf.stat.read().codeinreq().bit() {
            //let d = puf.codeoutput.read().bits();
            //acdata[idx] = d;
            puf.codeinput.write(|w| unsafe { w.codein().bits(activation_code[idx]) } );
            idx += 1;
        }
    }

    return puf.stat.read().success().bit();
}

fn puf_set_intrisic_key(puf : &device::puf::RegisterBlock, key_index: u8, key_size: u32, key_code: &mut [u32]) -> bool {
    let mut idx = 0;

    if ! puf.allow.read().allowsetkey().bit() {
        return false;
    }

    // The NXP C driver gives this in bytes(?) but giving this in bits
    // is much more obvious. key_size gets written as bits % 64 in the register
    // per table 48.10.7.3
    if key_size < 64 || key_size > 4096 || key_size % 64 != 0 {
        return false;
    }

    if key_index > 15 {
        return false;
    }

    puf.keysize.write(|w| unsafe { w.keysize().bits((key_size >> 6) as u8) });
    puf.keyindex.write(|w| unsafe { w.keyidx().bits(key_index) });

    puf.ctrl.write(|w| w.generatekey().set_bit());


    while ! puf.stat.read().busy().bit() && ! puf.stat.read().error().bit() { }


    while puf.stat.read().busy().bit() {
        if puf.stat.read().codeoutavail().bit() {
            let out = puf.codeoutput.read().bits();
            key_code[idx] = out;
            idx += 1;
        }
    }

    return puf.stat.read().success().bit();
}
