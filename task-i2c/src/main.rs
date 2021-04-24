//! I2C debugging cohort
//!
//! This task operates as a cohort for Humility to perform I2C operations.
//! These operations allow busses to be scanned for devices, a device
//! to be scanned for valid register values, and/or a particular register to
//! be read from or written to.  We would not expect this
//! task to be included in production systems.
//!
//! For example, to scan I2C1 via this cohort, one would execute `humility i2c`
//! this way:
//!
//! ```console
//! $ humility i2c -s -c 1
//! humility: attached via ST-Link
//!
//! Device scan on controller I2C1:
//!
//!    R = Reserved   - = No device   \o/ = Device found   X = Timed out
//!
//! ADDR     0x0 0x1 0x2 0x3 0x4 0x5 0x6 0x7 0x8 0x9 0xa 0xb 0xc 0xd 0xe 0xf
//! 0x00       R   R   R   R   R   R   R   R   -   -   -   -   -   -   -   -
//! 0x10       -   -   -   -   -   -   -   -   -   -   -   -   -   -   -   -
//! 0x20     \o/ \o/   -   -   -   -   -   -   -   -   -   -   -   -   -   -
//! 0x30       -   -   -   -   -   -   -   -   -   -   -   -   -   -   -   -
//! 0x40     \o/ \o/   -   -   -   -   -   -   -   -   -   -   -   -   -   -
//! 0x50       -   -   -   -   -   -   -   -   -   -   -   -   -   -   -   -
//! 0x60       -   -   -   -   -   -   -   -   -   -   -   -   -   -   -   -
//! 0x70       -   -   -   -   -   -   -   -   -   -   -   -   R   R   R   R
//! ```
//!

#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use drv_i2c_api::*;
use userlib::*;

#[cfg(feature = "standalone")]
const I2C: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

//
// Okay, don't judge, but these variables constitute an interface with
// Humility; don't change the semantics of these variables without also
// changing "humility i2c"!
//
#[no_mangle]
static mut I2C_DEBUG_RESULTS: [Option<Result<u8, ResponseCode>>; 256] =
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
static I2C_DEBUG_NBYTES: AtomicI32 = AtomicI32::new(-1);
static I2C_DEBUG_VALUE: AtomicI32 = AtomicI32::new(-1);

#[derive(Copy, Clone)]
enum BytesToRead {
    OneByte = 1,
    TwoBytes = 2,
}

fn scan_controller(
    controller: Controller,
    port: Port,
    mux: Option<(Mux, Segment)>,
) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let results = unsafe { &mut I2C_DEBUG_RESULTS };

    for addr in 0..128 {
        let device = I2cDevice::new(task, controller, port, mux, addr);
        let result = device.read::<u8>();
        results[addr as usize] = match result {
            Ok(result) => Some(Ok(result)),
            Err(err) => Some(Err(err)),
        };
    }
}

fn scan_device(
    controller: Controller,
    port: Port,
    mux: Option<(Mux, Segment)>,
    addr: u8,
) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let results = unsafe { &mut I2C_DEBUG_RESULTS };

    let device = I2cDevice::new(task, controller, port, mux, addr);

    for reg in 0..=0xff {
        let result = device.read_reg::<u8, u8>(reg);
        results[reg as usize] = match result {
            Ok(result) => Some(Ok(result)),
            Err(err) => Some(Err(err)),
        };
    }
}

fn read(
    controller: Controller,
    port: Port,
    mux: Option<(Mux, Segment)>,
    addr: u8,
    register: Option<u8>,
    nbytes: BytesToRead,
) {
    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let results = unsafe { &mut I2C_DEBUG_RESULTS };

    let device = I2cDevice::new(task, controller, port, mux, addr);

    match nbytes {
        BytesToRead::OneByte => {
            let result = match register {
                Some(register) => device.read_reg::<u8, u8>(register),
                None => device.read::<u8>(),
            };

            results[0] = match result {
                Ok(result) => Some(Ok(result)),
                Err(err) => Some(Err(err)),
            };
        }
        BytesToRead::TwoBytes => {
            let result = match register {
                Some(register) => device.read_reg::<u8, [u8; 2]>(register),
                None => device.read::<[u8; 2]>(),
            };

            match result {
                Ok(result) => {
                    results[0] = Some(Ok(result[0]));
                    results[1] = Some(Ok(result[1]));
                }
                Err(err) => {
                    results[0] = Some(Err(err));
                }
            }
        }
    }
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
    let results = unsafe { &mut I2C_DEBUG_RESULTS };

    let device = I2cDevice::new(task, controller, port, mux, addr);

    let mut buf = [0u8; 2];
    buf[0] = register;
    buf[1] = value;

    results[0] = match device.write(&buf) {
        Ok(_) => Some(Ok(value)),
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
            continue;
        }

        let results = unsafe { &mut I2C_DEBUG_RESULTS };

        for i in 0..results.len() {
            results[i] = None;
        }

        I2C_DEBUG_KICK.fetch_sub(1, Ordering::SeqCst);

        let controller = I2C_DEBUG_CONTROLLER.swap(-1, Ordering::SeqCst);

        let p = unsafe { &mut I2C_DEBUG_PORT };
        let port = *p;
        *p = Port::Default;

        let mux = I2C_DEBUG_MUX.swap(-1, Ordering::SeqCst);
        let segment = I2C_DEBUG_SEGMENT.swap(-1, Ordering::SeqCst);
        let device = I2C_DEBUG_DEVICE.swap(-1, Ordering::SeqCst);
        let reg = I2C_DEBUG_REGISTER.swap(-1, Ordering::SeqCst);
        let nbytes = I2C_DEBUG_NBYTES.swap(-1, Ordering::SeqCst);
        let value = I2C_DEBUG_VALUE.swap(-1, Ordering::SeqCst);

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
            Some((
                match Mux::from_i32(mux) {
                    Some(mux) => mux,
                    None => {
                        sys_log!("i2c_debug: invalid mux value {}", mux);
                        I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                },
                match Segment::from_i32(segment) {
                    Some(segment) => segment,
                    None => {
                        sys_log!(
                            "i2c_debug: invalid segment value {}",
                            segment
                        );
                        I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                },
            ))
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
            read(
                controller,
                port,
                mux,
                device as u8,
                if reg <= u8::MAX.into() {
                    Some(reg as u8)
                } else {
                    None
                },
                match nbytes {
                    -1 | 1 => BytesToRead::OneByte,
                    2 => BytesToRead::TwoBytes,
                    _ => {
                        sys_log!("i2c_debug: invalid nbytes value {}", nbytes);
                        I2C_DEBUG_ERRORS.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                },
            );

            I2C_DEBUG_REQUESTS.fetch_add(1, Ordering::SeqCst);
            continue;
        }

        write_register(
            controller,
            port,
            mux,
            device as u8,
            reg as u8,
            value as u8,
        );
        I2C_DEBUG_REQUESTS.fetch_add(1, Ordering::SeqCst);
    }
}
