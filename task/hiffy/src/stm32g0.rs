// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use hif::*;
#[cfg(feature = "send")]
use hubris_num_tasks::Task;

#[cfg(any(feature = "gpio", feature = "i2c"))]
use userlib::*;

#[cfg(feature = "gpio")]
task_slot!(SYS, sys);

#[cfg(feature = "i2c")]
use drv_i2c_api::{
    Controller, I2cDevice, Mux, PortIndex, ResponseCode, Segment,
};

#[cfg(feature = "i2c")]
task_slot!(I2C, i2c_driver);

// TODO: this type is copy-pasted in several modules
pub struct Buffer(#[allow(dead_code)] u8);

//
// The order in this enum must match the order in the functions array that
// is passed to execute.
//
pub enum Functions {
    Sleep(u16, u32),
    #[cfg(feature = "send")]
    Send((Task, u16, Buffer, usize), u32),
    #[cfg(feature = "send")]
    SendLeaseRead((Task, u16, Buffer, usize, usize), u32),
    #[cfg(feature = "send-rw")]
    SendLeaseReadWrite((Task, u16, Buffer, usize, usize, usize), u32),
    #[cfg(feature = "send")]
    SendLeaseWrite((Task, u16, Buffer, usize, usize), u32),
    #[cfg(feature = "i2c")]
    I2cRead(
        (Controller, PortIndex, Mux, Segment, u8, u8, usize),
        ResponseCode,
    ),
    #[cfg(feature = "i2c")]
    I2cWrite(
        (Controller, PortIndex, Mux, Segment, u8, u8, Buffer, usize),
        ResponseCode,
    ),
    #[cfg(feature = "i2c")]
    I2cBulkWrite(
        (Controller, PortIndex, Mux, Segment, u8, u8, usize, usize),
        ResponseCode,
    ),
    #[cfg(feature = "gpio")]
    GpioInput(drv_stm32xx_sys_api::Port, u32),
    #[cfg(feature = "gpio")]
    GpioToggle((drv_stm32xx_sys_api::Port, u8), u32),
    #[cfg(feature = "gpio")]
    GpioSet((drv_stm32xx_sys_api::Port, u8), u32),
    #[cfg(feature = "gpio")]
    GpioReset((drv_stm32xx_sys_api::Port, u8), u32),
    #[cfg(feature = "gpio")]
    GpioConfigure(
        (
            drv_stm32xx_sys_api::Port,
            u8,
            drv_stm32xx_sys_api::Mode,
            drv_stm32xx_sys_api::OutputType,
            drv_stm32xx_sys_api::Speed,
            drv_stm32xx_sys_api::Pull,
            drv_stm32xx_sys_api::Alternate,
        ),
        u32,
    ),
}

/// Expected stack frame:
/// ```text
/// [0] controller index (integer)
/// [1] port index (integer)
/// [2] mux index (optional integer)
/// [3] segment index (optional integer)
/// [4] i2c address (integer)
/// [5] register index (optional integer)
/// ```
#[cfg(feature = "i2c")]
fn i2c_args_to_device(
    stack: &[Option<u32>],
) -> Result<(I2cDevice, Option<u8>), Failure> {
    let controller =
        Controller::from_u32(stack[0].ok_or(Fault::EmptyParameter(0))?)
            .ok_or(Fault::BadParameter(0))?;

    //
    // While we once upon a time allowed HIF consumers to specify a default
    // port, we now expect all HIF consumers to read the device configuration
    // and correctly specify a port index: this is an error.
    //
    let port = stack[1].ok_or(Fault::EmptyParameter(1))?;
    let port = if port > u8::MAX.into() {
        return Err(Failure::Fault(Fault::BadParameter(1)));
    } else {
        PortIndex(port as u8)
    };

    let mux = match (stack[2], stack[3]) {
        (Some(mux), Some(segment)) => Some((
            Mux::from_u32(mux).ok_or(Fault::BadParameter(2))?,
            Segment::from_u32(segment).ok_or(Fault::BadParameter(3))?,
        )),
        _ => None,
    };

    let addr = stack[4].ok_or(Fault::EmptyParameter(4))? as u8;
    let register = stack[5].map(|r| r as u8);

    let task = I2C.get_task_id();
    Ok((I2cDevice::new(task, controller, port, mux, addr), register))
}

#[cfg(feature = "i2c")]
fn i2c_read(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.len() < 7 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 7;
    let (device, register) = i2c_args_to_device(&stack[fp..])?;

    match stack[fp + 6] {
        Some(nbytes) => {
            let n = nbytes as usize;

            let Some(buf) = rval.get_mut(..n) else {
                return Err(Failure::Fault(Fault::ReturnValueOverflow));
            };

            let res = if let Some(reg) = register {
                device.read_reg_into::<u8>(reg, buf)
            } else {
                device.read_into(buf)
            };
            res.map_err(|e| Failure::FunctionError(u32::from(e)))
        }

        None => {
            if let Some(reg) = register {
                let Some(buf) = rval.get_mut(..256) else {
                    return Err(Failure::Fault(Fault::ReturnValueOverflow));
                };

                device
                    .read_block::<u8>(reg, buf)
                    .map_err(|e| Failure::FunctionError(u32::from(e)))
            } else {
                Err(Failure::Fault(Fault::EmptyParameter(6)))
            }
        }
    }
}

#[cfg(feature = "i2c")]
fn i2c_write(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let mut buf = [0u8; 17];

    //
    // We need at least 8 (!) parameters, the last of which is the number of
    // bytes to write.
    //
    if stack.len() < 8 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let len = match stack[stack.len() - 1] {
        Some(len) if len > 0 && (len as usize) < buf.len() => Ok(len as usize),
        _ => Err(Failure::Fault(Fault::BadParameter(7))),
    }?;
    let frame_size = len.checked_add(7).ok_or(Fault::BadParameter(7))?;

    let fp = stack
        .len()
        .checked_sub(frame_size)
        .ok_or(Fault::MissingParameters)?;
    let frame = &stack[fp..];
    let (device, register) = i2c_args_to_device(frame)?;

    let mut offs = 0;

    if let Some(register) = register {
        buf[offs] = register;
        offs += 1;
    }

    let bp = 6;
    let bufsec = &mut buf[..len + offs];

    for i in 0..len {
        bufsec[i + offs] = frame[bp + i].ok_or(Fault::BadParameter(7))? as u8;
    }

    match device.write(bufsec) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "i2c")]
fn i2c_bulk_write(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    //
    // We need exactly 8 parameters: the normal i2c parameters (controller,
    // port, mux, segment, address, register) plus the offset and length.
    // Note that the register must be None.
    //
    if stack.len() != 8 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let offset = match stack[stack.len() - 2] {
        Some(offset) if (offset as usize) < data.len() => Ok(offset as usize),
        _ => Err(Failure::Fault(Fault::BadParameter(6))),
    }?;

    let len = match stack[stack.len() - 1] {
        Some(len) if len > 0 && offset + (len as usize) < data.len() => {
            Ok(len as usize)
        }
        _ => Err(Failure::Fault(Fault::BadParameter(7))),
    }?;

    let fp = stack.len() - 8;
    let (device, register) = i2c_args_to_device(&stack[fp..])?;

    if register.is_some() {
        return Err(Failure::Fault(Fault::BadParameter(5)));
    }

    match device.write(&data[offset..offset + len]) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_args(
    stack: &[Option<u32>],
) -> Result<(drv_stm32xx_sys_api::Port, u16), Failure> {
    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 2;

    let port = match stack[fp + 0] {
        Some(port) => match drv_stm32xx_sys_api::Port::from_u32(port) {
            Some(port) => port,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    let mask = match stack[fp + 1] {
        Some(pin) if pin < 16 => 1u16 << pin,
        Some(_) => {
            return Err(Failure::Fault(Fault::BadParameter(1)));
        }
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(1)));
        }
    };

    Ok((port, mask))
}

#[cfg(feature = "gpio")]
fn gpio_input(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use byteorder::ByteOrder;

    let task = SYS.get_task_id();
    let sys = drv_stm32xx_sys_api::Sys::from(task);

    if stack.is_empty() {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 1;

    let port = match stack[fp + 0] {
        Some(port) => match drv_stm32xx_sys_api::Port::from_u32(port) {
            Some(port) => port,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    let input = sys.gpio_read_input(port);

    byteorder::LittleEndian::write_u16(rval, input);
    Ok(core::mem::size_of::<u16>())
}

#[cfg(feature = "gpio")]
fn gpio_toggle(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = SYS.get_task_id();
    let sys = drv_stm32xx_sys_api::Sys::from(task);

    let (port, mask) = gpio_args(stack)?;

    match sys.gpio_toggle(port, mask) {
        Ok(_) => Ok(0),
        Err(idol_runtime::ServerDeath) => panic!(),
    }
}

#[cfg(feature = "gpio")]
fn gpio_set(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = SYS.get_task_id();
    let sys = drv_stm32xx_sys_api::Sys::from(task);

    let (port, mask) = gpio_args(stack)?;

    sys.gpio_set_reset(port, mask, 0);

    Ok(0)
}

#[cfg(feature = "gpio")]
fn gpio_reset(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = SYS.get_task_id();
    let sys = drv_stm32xx_sys_api::Sys::from(task);

    let (port, mask) = gpio_args(stack)?;

    sys.gpio_set_reset(port, 0, mask);

    Ok(0)
}

#[cfg(feature = "gpio")]
fn gpio_configure(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_stm32xx_sys_api::*;

    if stack.len() < 7 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 7;
    let (port, mask) = gpio_args(&stack[fp..fp + 2])?;

    let mode = match stack[fp + 2] {
        Some(mode) => match Mode::from_u32(mode) {
            Some(mode) => mode,
            None => return Err(Failure::Fault(Fault::BadParameter(2))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(2))),
    };

    let output_type = match stack[fp + 3] {
        Some(output_type) => match OutputType::from_u32(output_type) {
            Some(output_type) => output_type,
            None => return Err(Failure::Fault(Fault::BadParameter(3))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(3))),
    };

    let speed = match stack[fp + 4] {
        Some(speed) => match Speed::from_u32(speed) {
            Some(speed) => speed,
            None => return Err(Failure::Fault(Fault::BadParameter(4))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(4))),
    };

    let pull = match stack[fp + 5] {
        Some(pull) => match Pull::from_u32(pull) {
            Some(pull) => pull,
            None => return Err(Failure::Fault(Fault::BadParameter(5))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(5))),
    };

    let af = match stack[fp + 6] {
        Some(af) => match Alternate::from_u32(af) {
            Some(af) => af,
            None => return Err(Failure::Fault(Fault::BadParameter(6))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(6))),
    };

    let task = SYS.get_task_id();
    let sys = drv_stm32xx_sys_api::Sys::from(task);

    sys.gpio_configure(port, mask, mode, output_type, speed, pull, af);

    Ok(0)
}

pub(crate) static HIFFY_FUNCS: &[Function] = &[
    crate::common::sleep,
    #[cfg(feature = "send")]
    crate::common::send,
    #[cfg(feature = "send")]
    crate::common::send_lease_read,
    #[cfg(feature = "send-rw")]
    crate::common::send_lease_read_write,
    #[cfg(feature = "send")]
    crate::common::send_lease_write,
    #[cfg(feature = "i2c")]
    i2c_read,
    #[cfg(feature = "i2c")]
    i2c_write,
    #[cfg(feature = "i2c")]
    i2c_bulk_write,
    #[cfg(feature = "gpio")]
    gpio_input,
    #[cfg(feature = "gpio")]
    gpio_toggle,
    #[cfg(feature = "gpio")]
    gpio_set,
    #[cfg(feature = "gpio")]
    gpio_reset,
    #[cfg(feature = "gpio")]
    gpio_configure,
];

//
// This definition forces the compiler to emit the DWARF needed for debuggers
// to be able to know function indices, arguments and return values.
//
#[no_mangle]
#[used(compiler)]
static HIFFY_FUNCTIONS: Option<&Functions> = None;

pub(crate) fn trace_execute(_offset: usize, _op: hif::Op) {}

pub(crate) fn trace_success() {}

pub(crate) fn trace_failure(_f: hif::Failure) {}
