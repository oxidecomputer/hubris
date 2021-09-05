//! HIF interpreter
//!
//! HIF is the Hubris/Humility Interchange Format, a simple stack-based
//! machine that allows for some dynamic programmability of Hubris.  In
//! particular, this task provides a HIF interpreter to allow for Humility
//! commands like `humility i2c`, `humility pmbus` and `humility jefe`.  The
//! debugger places HIF in [`HIFFY_TEXT`], and then indicates that text is
//! present by incrementing [`HIFFY_KICK`].  This task executes the specified
//! HIF, with the return stack located in [`HIFFY_RSTACK`].

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use hif::*;
use ringbuf::*;
use task_jefe_api::{Disposition, Jefe, JefeError};
use userlib::*;

cfg_if::cfg_if! {
    if #[cfg(feature = "i2c")] {
        use drv_i2c_api::{Controller, I2cDevice, Mux, Port, ResponseCode, Segment};

        #[cfg(feature = "standalone")]
        const I2C: Task = Task::anonymous;

        #[cfg(not(feature = "standalone"))]
        const I2C: Task = Task::i2c_driver;
    }
}

const JEFE: Task = Task::jefe;

#[no_mangle]
static mut HIFFY_TEXT: [u8; 2048] = [0; 2048];
static mut HIFFY_RSTACK: [u8; 2048] = [0; 2048];
static HIFFY_REQUESTS: AtomicU32 = AtomicU32::new(0);
static HIFFY_ERRORS: AtomicU32 = AtomicU32::new(0);
static HIFFY_KICK: AtomicU32 = AtomicU32::new(0);
static HIFFY_READY: AtomicU32 = AtomicU32::new(0);

static HIFFY_VERSION_MAJOR: AtomicU32 = AtomicU32::new(HIF_VERSION_MAJOR);
static HIFFY_VERSION_MINOR: AtomicU32 = AtomicU32::new(HIF_VERSION_MINOR);
static HIFFY_VERSION_PATCH: AtomicU32 = AtomicU32::new(HIF_VERSION_PATCH);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Execute((usize, Op)),
    Failure(Failure),
    Success,
    None,
}

ringbuf!(Trace, 64, Trace::None);

pub struct Buffer(u8);

//
// The order in this enum must match the order in the functions array that
// is passed to execute.
//
pub enum Functions {
    #[cfg(feature = "i2c")]
    I2cRead(
        (Controller, Port, Mux, Segment, u8, u8, usize),
        ResponseCode,
    ),
    #[cfg(feature = "i2c")]
    I2cWrite(
        (Controller, Port, Mux, Segment, u8, u8, Buffer, usize),
        ResponseCode,
    ),
    JefeSetDisposition((u16, Disposition), JefeError),
}

//
// This definition forces the compiler to emit the DWARF needed for debuggers
// to be able to know function indices, arguments and return values.
//
#[no_mangle]
static HIFFY_FUNCTIONS: Option<&Functions> = None;

#[cfg(feature = "i2c")]
fn i2c_args(
    stack: &[Option<u32>],
) -> Result<(Controller, Port, Option<(Mux, Segment)>, u8, Option<u8>), Failure>
{
    let controller = match stack[0] {
        Some(controller) => match Controller::from_u32(controller) {
            Some(controller) => controller,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    let port = match stack[1] {
        Some(port) => match Port::from_u32(port) {
            Some(port) => port,
            None => {
                return Err(Failure::Fault(Fault::BadParameter(1)));
            }
        },
        None => Port::Default,
    };

    let mux = match (stack[2], stack[3]) {
        (Some(mux), Some(segment)) => Some((
            match Mux::from_u32(mux) {
                Some(mux) => mux,
                None => {
                    return Err(Failure::Fault(Fault::BadParameter(2)));
                }
            },
            match Segment::from_u32(segment) {
                Some(segment) => segment,
                None => {
                    return Err(Failure::Fault(Fault::BadParameter(3)));
                }
            },
        )),
        _ => None,
    };

    let addr = match stack[4] {
        Some(addr) => addr as u8,
        None => return Err(Failure::Fault(Fault::EmptyParameter(4))),
    };

    let register = match stack[5] {
        Some(register) => Some(register as u8),
        None => None,
    };

    Ok((controller, port, mux, addr, register))
}

#[cfg(feature = "i2c")]
fn i2c_read(stack: &[Option<u32>], rval: &mut [u8]) -> Result<usize, Failure> {
    if stack.len() < 7 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 7;
    let (controller, port, mux, addr, register) = i2c_args(&stack[fp..])?;

    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let device = I2cDevice::new(task, controller, port, mux, addr);

    match stack[fp + 6] {
        Some(nbytes) => {
            let n = nbytes as usize;

            if rval.len() < n {
                return Err(Failure::Fault(Fault::ReturnValueOverflow));
            }

            let res = if let Some(reg) = register {
                device.read_reg_into::<u8>(reg as u8, &mut rval[0..n])
            } else {
                device.read_into(&mut rval[0..n])
            };

            match res {
                Ok(rlen) => Ok(rlen),
                Err(err) => Err(Failure::FunctionError(err.into())),
            }
        }

        None => {
            if let Some(reg) = register {
                if rval.len() < 256 {
                    return Err(Failure::Fault(Fault::ReturnValueOverflow));
                }

                match device.read_block::<u8>(reg as u8, &mut rval[0..0xff]) {
                    Ok(rlen) => Ok(rlen),
                    Err(err) => Err(Failure::FunctionError(err.into())),
                }
            } else {
                Err(Failure::Fault(Fault::EmptyParameter(6)))
            }
        }
    }
}

#[cfg(feature = "i2c")]
fn i2c_write(
    stack: &[Option<u32>],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let mut buf = [0u8; 5];

    //
    // We need at least 8 (!) parameters, the last of which is the number of
    // bytes to write.
    //
    if stack.len() < 8 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let len = match stack[stack.len() - 1] {
        Some(len) if len > 0 && len as usize <= buf.len() - 1 => {
            Ok(len as usize)
        }
        _ => Err(Failure::Fault(Fault::BadParameter(7))),
    }?;

    if stack.len() < 7 + len {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - (7 + len);
    let (controller, port, mux, addr, register) = i2c_args(&stack[fp..])?;

    let task = TaskId::for_index_and_gen(I2C as usize, Generation::default());
    let device = I2cDevice::new(task, controller, port, mux, addr);

    let mut offs = 0;

    if let Some(register) = register {
        buf[offs] = register;
        offs += 1;
    }

    let bp = stack.len() - (1 + len);

    for i in 0..len {
        buf[i + offs] = match stack[bp + i] {
            None => {
                return Err(Failure::Fault(Fault::BadParameter(7)));
            }
            Some(val) => val as u8,
        }
    }

    match device.write(&buf[0..len + offs]) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

fn jefe_set_disposition(
    stack: &[Option<u32>],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 2;

    let task = match stack[fp + 0] {
        Some(task) => task as u16,
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    let disposition = match stack[fp + 1] {
        Some(disposition) => match Disposition::from_u32(disposition) {
            Some(disposition) => disposition,
            None => {
                return Err(Failure::Fault(Fault::BadParameter(1)));
            }
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(1))),
    };

    let jefe = Jefe(TaskId::for_index_and_gen(
        JEFE as usize,
        Generation::default(),
    ));

    match jefe.set_disposition(TaskId(task), disposition) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut sleep_ms = 250;
    let mut sleeps = 0;
    let mut stack = [None; 32];
    let mut scratch = [0u8; 256];
    const NLABELS: usize = 4;

    let functions: &[Function] = &[
        #[cfg(feature = "i2c")]
        i2c_read,
        #[cfg(feature = "i2c")]
        i2c_write,
        jefe_set_disposition,
    ];

    //
    // Sadly, there seems to be no other way to force these variables to
    // not be eliminated...
    //
    HIFFY_VERSION_MAJOR.fetch_add(0, Ordering::SeqCst);
    HIFFY_VERSION_MINOR.fetch_add(0, Ordering::SeqCst);
    HIFFY_VERSION_PATCH.fetch_add(0, Ordering::SeqCst);

    loop {
        HIFFY_READY.fetch_add(1, Ordering::SeqCst);
        hl::sleep_for(sleep_ms);
        HIFFY_READY.fetch_sub(1, Ordering::SeqCst);

        if HIFFY_KICK.load(Ordering::SeqCst) == 0 {
            sleeps += 1;

            // Exponentially backoff our sleep value, but no more than 250ms
            if sleeps == 10 {
                sleep_ms = core::cmp::min(sleep_ms * 10, 250);
                sleeps = 0;
            }

            continue;
        }

        //
        // Whenever we have been kicked, we adjust our timeout down to 1ms,
        // from which we will exponentially backoff
        //
        HIFFY_KICK.fetch_sub(1, Ordering::SeqCst);
        sleep_ms = 1;
        sleeps = 0;

        let text = unsafe { &HIFFY_TEXT };
        let mut rstack = unsafe { &mut HIFFY_RSTACK[0..] };

        let check = |offset: usize, op: &Op| -> Result<(), Failure> {
            ringbuf_entry!(Trace::Execute((offset, *op)));
            Ok(())
        };

        let rv = execute::<_, NLABELS>(
            text,
            functions,
            &mut stack,
            &mut rstack,
            &mut scratch,
            check,
        );

        match rv {
            Ok(_) => {
                HIFFY_REQUESTS.fetch_add(1, Ordering::SeqCst);
                ringbuf_entry!(Trace::Success);
            }
            Err(failure) => {
                HIFFY_ERRORS.fetch_add(1, Ordering::SeqCst);
                ringbuf_entry!(Trace::Failure(failure));
            }
        }
    }
}
