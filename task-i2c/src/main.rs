#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use drv_i2c_api::*;
use userlib::*;

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
static mut I2C_DEBUG_RESULTS: [Option<Result<u32, ResponseCode>>; 256] =
    [None; 256];
static I2C_DEBUG_REQUESTS: AtomicU32 = AtomicU32::new(0);
static I2C_DEBUG_ERRORS: AtomicU32 = AtomicU32::new(0);
static I2C_DEBUG_KICK: AtomicU32 = AtomicU32::new(0);
static I2C_DEBUG_READY: AtomicU32 = AtomicU32::new(0);
static I2C_DEBUG_CONTROLLER: AtomicI32 = AtomicI32::new(-1);

#[no_mangle]
static mut I2C_DEBUG_PORT: Port = Port::Default;

static I2C_DEBUG_MUX: AtomicI32 = AtomicI32::new(-1);
static I2C_DEBUG_SEGMENT: AtomicI32 = AtomicI32::new(-1);
static I2C_DEBUG_DEVICE: AtomicI32 = AtomicI32::new(-1);
static I2C_DEBUG_REGISTER: AtomicI32 = AtomicI32::new(-1);
static I2C_DEBUG_VALUE: AtomicI32 = AtomicI32::new(-1);

fn scan_controller(
    controller: Controller,
    port: Port,
    mux: Option<(Mux, Segment)>
) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let mut results = unsafe { &mut I2C_DEBUG_RESULTS };

    sys_log!("i2c_debug: scanning controller {:?}", controller);

    for addr in 0..128 {
        let i2c = I2c::new(task, controller, port, mux, addr);
        let result = i2c.read_reg::<u8, u8>(0);
        results[addr as usize] = match result {
            Ok(result) => Some(Ok(result as u32)),
            Err(err) => Some(Err(err)),
        };
    }
}

fn scan_device(
    controller: Controller,
    port: Port,
    mux: Option<(Mux, Segment)>,
    addr: u8
) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let mut results = unsafe { &mut I2C_DEBUG_RESULTS };

    sys_log!(
        "i2c_debug: scanning controller {:?}, addr 0x{:x}, mux {:?}",
        controller,
        addr,
        mux
    );

    let i2c = I2c::new(task, controller, port, mux, addr);

    for reg in 0..=0xff {
        let result = i2c.read_reg::<u8, u8>(reg);
        results[reg as usize] = match result {
            Ok(result) => Some(Ok(result as u32)),
            Err(err) => Some(Err(err)),
        };
    }
}

fn read_register(
    controller: Controller,
    port: Port,
    mux: Option<(Mux, Segment)>,
    addr: u8,
    register: u8
) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let mut results = unsafe { &mut I2C_DEBUG_RESULTS };

    sys_log!(
        "i2c_debug: reading {:?}, addr 0x{:x}, register 0x{:x}",
        controller,
        addr,
        register
    );

    let i2c = I2c::new(task, controller, port, mux, addr);

    results[0] = match i2c.read_reg::<u8, u8>(register) {
        Ok(result) => Some(Ok(result as u32)),
        Err(err) => Some(Err(err)),
    };
}

fn write_register(
    controller: Controller,
    port: Port,
    mux: Option<(Mux, Segment)>,
    addr: u8,
    register: u8,
    value: u8,
) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let mut results = unsafe { &mut I2C_DEBUG_RESULTS };

    sys_log!(
        "i2c_debug: writing {:?}, addr 0x{:x}, register 0x{:x}: 0x{:x}",
        controller,
        addr,
        register,
        value
    );

    let i2c = I2c::new(task, controller, port, mux, addr);

    let mut buf = [0u8; 2];
    buf[0] = register;
    buf[1] = value;

    results[0] = match i2c.write(&buf) {
        Ok(_) => Some(Ok(value as u32)),
        Err(err) => Some(Err(err)),
    };
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

        let controller = I2C_DEBUG_CONTROLLER.swap(-1, Ordering::SeqCst);

        let mut p = unsafe { &mut I2C_DEBUG_PORT };
        let port = *p;
        *p = Port::Default;

        let mux = I2C_DEBUG_MUX.swap(-1, Ordering::SeqCst);
        let segment = I2C_DEBUG_SEGMENT.swap(-1, Ordering::SeqCst);
        let device = I2C_DEBUG_DEVICE.swap(-1, Ordering::SeqCst);
        let reg = I2C_DEBUG_REGISTER.swap(-1, Ordering::SeqCst);
        let value = I2C_DEBUG_VALUE.swap(-1, Ordering::SeqCst);

        sys_log!("i2c_debug: controller={}, \
            port={:?}, mux={:?}, device=0x{:x}, register=0x{:x}",
            controller, port, mux, device, reg);

        if controller == -1 {
            sys_log!("i2c_debug: controller must be set");
            I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        let controller = match Controller::from_i32(controller) {
            Some(controller) => controller,
            None => {
                sys_log!("i2c_debug: invalid controller value {}", controller);
                I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
                continue;
            }
        };

        let mux = if mux != -1 && segment != -1 {
            Some((match Mux::from_i32(mux) {
                Some(mux) => mux,
                None => {
                    sys_log!("i2c_debug: invalid mux value {}", mux);
                    I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
            }, match Segment::from_i32(segment) {
                Some(segment) => segment,
                None => {
                    sys_log!("i2c_debug: invalid segment value {}", segment);
                    I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
            }))
        } else {
            None
        };

        if device == -1 {
            scan_controller(controller, port, mux);
            I2C_DEBUG_REQUESTS.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        if reg == -1 {
            scan_device(controller, port, mux, device as u8);
            I2C_DEBUG_REQUESTS.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        if value == -1 {
            read_register(controller, port, mux, device as u8, reg as u8);
            I2C_DEBUG_REQUESTS.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        write_register(controller, port, mux, device as u8, reg as u8, value as u8);
        I2C_DEBUG_REQUESTS.fetch_add(1, Ordering::SeqCst);
    }
}
