#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use userlib::*;
use drv_i2c_api::*;
use core::sync::atomic::{AtomicU32, AtomicI32, Ordering};

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

//
// Okay, don't judge, but these variables constitute an interface with
// Humility; don't change the semantics of these variables without also
// changing "humility i2c"!
//
#[no_mangle]
static mut I2C_DEBUG_RESULTS: [Option<Result<u32, I2cError>>; 256] = [None; 256];
static I2C_DEBUG_REQUESTS: AtomicU32 = AtomicU32::new(0);
static I2C_DEBUG_ERRORS: AtomicU32 = AtomicU32::new(0);
static I2C_DEBUG_KICK: AtomicU32 = AtomicU32::new(0);
static I2C_DEBUG_READY: AtomicU32 = AtomicU32::new(0);
static I2C_DEBUG_BUS: AtomicI32 = AtomicI32::new(-1);
static I2C_DEBUG_DEVICE: AtomicI32 = AtomicI32::new(-1);
static I2C_DEBUG_REGISTER: AtomicI32 = AtomicI32::new(-1);

fn scan_bus(interface: Interface) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let mut results = unsafe { &mut I2C_DEBUG_RESULTS };

    sys_log!("i2c_debug: scanning bus {:?}", interface);

    for addr in 0..128 {
        let i2c = I2c::new(task, interface, addr);
        let result = i2c.read_reg::<u8, u8>(0);
        results[addr as usize] = match result {
            Ok(result) => { Some(Ok(result as u32)) },
            Err(err) => { Some(Err(err)) }
        };
    }
}

fn scan_device(interface: Interface, addr: u8) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let mut results = unsafe { &mut I2C_DEBUG_RESULTS };

    sys_log!("i2c_debug: scanning bus {:?}, addr 0x{:x}", interface, addr);

    let i2c = I2c::new(task, interface, addr);

    for reg in 0..=0xff {
        let result = i2c.read_reg::<u8, u8>(reg);
        results[reg as usize] = match result {
            Ok(result) => { Some(Ok(result as u32)) },
            Err(err) => { Some(Err(err)) }
        };
    }
}

#[export_name = "main"]
fn main() -> ! {
    loop {
        I2C_DEBUG_READY.fetch_add(1, Ordering::SeqCst);
        hl::sleep_for(1000);
        I2C_DEBUG_READY.fetch_sub(1, Ordering::SeqCst);

        if I2C_DEBUG_KICK.load(Ordering::SeqCst) == 0 {
            sys_log!("i2c_debug: nothing to do");
            continue;
        }

        let mut results = unsafe { &mut I2C_DEBUG_RESULTS };

        for i in 0..results.len() {
            results[i] = None;
        }

        I2C_DEBUG_KICK.fetch_sub(1, Ordering::SeqCst);

        let bus = I2C_DEBUG_BUS.swap(-1, Ordering::SeqCst);
        let device = I2C_DEBUG_DEVICE.swap(-1, Ordering::SeqCst);
        let register = I2C_DEBUG_REGISTER.swap(-1, Ordering::SeqCst);

        sys_log!("i2c_debug: bus={}, device=0x{:x}, register=0x{:x}",
            bus, device, register);

        if bus == -1 {
            sys_log!("i2c_debug: bus must be set");
            I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        let interface = match Interface::from_i32(bus) {
            Some(interface) => { interface }
            None => {
                sys_log!("i2c_debug: invalid bus value {}", bus);
                I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
                continue;
            }
        };

        if device == -1 {
            scan_bus(interface);
            I2C_DEBUG_REQUESTS.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        if register == -1 {
            scan_device(interface, device as u8);
            I2C_DEBUG_REQUESTS.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        sys_log!("i2c_debug: register reading not yet implemented");
        I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
    }
}
