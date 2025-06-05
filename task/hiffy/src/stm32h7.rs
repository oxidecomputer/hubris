// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#[cfg(feature = "hash")]
use crate::common::{
    hash_digest_sha256, hash_finalize_sha256, hash_init_sha256, hash_update,
};
#[cfg(feature = "spi")]
use crate::common::{spi_read, spi_write};
use hif::*;
use hubris_num_tasks::Task;
use ringbuf::*;
#[cfg(any(feature = "spi", feature = "gpio", feature = "i2c"))]
use userlib::*;

#[cfg(feature = "i2c")]
use drv_i2c_api::{
    Controller, I2cDevice, Mux, PortIndex, ResponseCode, Segment,
};

#[cfg(feature = "i2c")]
task_slot!(I2C, i2c_driver);

#[cfg(feature = "gpio")]
task_slot!(SYS, sys);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Execute((usize, Op)),
    Failure(Failure),
    #[cfg(feature = "gpio")]
    GpioConfigure(
        drv_stm32xx_sys_api::Port,
        u16,
        drv_stm32xx_sys_api::Mode,
        drv_stm32xx_sys_api::OutputType,
        drv_stm32xx_sys_api::Speed,
        drv_stm32xx_sys_api::Pull,
        drv_stm32xx_sys_api::Alternate,
    ),
    #[cfg(feature = "gpio")]
    GpioInput(drv_stm32xx_sys_api::Port),
    Success,
    None,
}

ringbuf!(Trace, 64, Trace::None);

// Field is only used in the debugger, appears dead to the compiler.
pub struct Buffer(#[allow(dead_code)] u8);

//
// The order in this enum must match the order in the functions array that
// is passed to execute.
//
pub enum Functions {
    Sleep(u16, u32),
    Send((Task, u16, Buffer, usize), u32),
    SendLeaseRead((Task, u16, Buffer, usize, usize), u32),
    SendLeaseReadWrite((Task, u16, Buffer, usize, usize, usize), u32),
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
    #[cfg(feature = "spi")]
    SpiRead((Task, u8, usize, usize), drv_spi_api::SpiError),
    #[cfg(feature = "spi")]
    SpiWrite((Task, u8, usize), drv_spi_api::SpiError),
    #[cfg(feature = "qspi")]
    QspiReadId((), drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiReadStatus((), drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiBulkErase((), drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiPageProgram((u32, usize, usize), drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiPageProgramSector0((u32, usize, usize), drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiRead((u32, usize), drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiSectorErase(u32, drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiSector0Erase((), drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiVerify((u32, usize, usize), drv_hf_api::HfError),
    #[cfg(feature = "qspi")]
    QspiHash((u32, u32), drv_hf_api::HfError),
    #[cfg(feature = "hash")]
    HashDigest(u32, drv_hash_api::HashError),
    #[cfg(feature = "hash")]
    HashInit((), drv_hash_api::HashError),
    #[cfg(feature = "hash")]
    HashUpdate(u32, drv_hash_api::HashError),
    #[cfg(feature = "hash")]
    HashFinalize((), drv_hash_api::HashError),
}

#[cfg(feature = "i2c")]
#[allow(clippy::type_complexity)] // TODO - type is indeed not fantastic
fn i2c_args(
    stack: &[Option<u32>],
) -> Result<
    (
        Controller,
        PortIndex,
        Option<(Mux, Segment)>,
        u8,
        Option<u8>,
    ),
    Failure,
> {
    let controller = match stack[0] {
        Some(controller) => match Controller::from_u32(controller) {
            Some(controller) => controller,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    let port = match stack[1] {
        Some(port) => {
            if port > u8::MAX.into() {
                return Err(Failure::Fault(Fault::BadParameter(1)));
            }

            PortIndex(port as u8)
        }
        None => {
            //
            // While we once upon a time allowed HIF consumers to specify
            // a default port, we now expect all HIF consumers to read the
            // device configuration and correctly specify a port index:
            // this is an error.
            //
            return Err(Failure::Fault(Fault::EmptyParameter(1)));
        }
    };

    let mux = match (stack[2], stack[3]) {
        (Some(mux), Some(segment)) => Some((
            Mux::from_u32(mux).ok_or(Failure::Fault(Fault::BadParameter(2)))?,
            Segment::from_u32(segment)
                .ok_or(Failure::Fault(Fault::BadParameter(3)))?,
        )),
        _ => None,
    };

    let addr = match stack[4] {
        Some(addr) => addr as u8,
        None => return Err(Failure::Fault(Fault::EmptyParameter(4))),
    };

    let register = stack[5].map(|r| r as u8);

    Ok((controller, port, mux, addr, register))
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
    let (controller, port, mux, addr, register) = i2c_args(&stack[fp..])?;

    let task = I2C.get_task_id();
    let device = I2cDevice::new(task, controller, port, mux, addr);

    match stack[fp + 6] {
        Some(nbytes) => {
            let n = nbytes as usize;

            if rval.len() < n {
                return Err(Failure::Fault(Fault::ReturnValueOverflow));
            }

            let res = if let Some(reg) = register {
                device.read_reg_into::<u8>(reg, &mut rval[0..n])
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

                match device.read_block::<u8>(reg, &mut rval[0..0xff]) {
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

    if stack.len() < 7 + len {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - (7 + len);
    let (controller, port, mux, addr, register) = i2c_args(&stack[fp..])?;

    let task = I2C.get_task_id();
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
    let (controller, port, mux, addr, register) = i2c_args(&stack[fp..])?;

    if register.is_some() {
        return Err(Failure::Fault(Fault::BadParameter(5)));
    }

    let task = I2C.get_task_id();
    let device = I2cDevice::new(task, controller, port, mux, addr);

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
    let gpio = drv_stm32xx_sys_api::Sys::from(task);

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

    ringbuf_entry!(Trace::GpioInput(port));

    let input = gpio.gpio_read_input(port);

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
    let gpio = drv_stm32xx_sys_api::Sys::from(task);

    let (port, mask) = gpio_args(stack)?;

    match gpio.gpio_toggle(port, mask) {
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
    let gpio = drv_stm32xx_sys_api::Sys::from(task);

    let (port, mask) = gpio_args(stack)?;

    gpio.gpio_set_reset(port, mask, 0);

    Ok(0)
}

#[cfg(feature = "gpio")]
fn gpio_reset(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = SYS.get_task_id();
    let gpio = drv_stm32xx_sys_api::Sys::from(task);

    let (port, mask) = gpio_args(stack)?;

    gpio.gpio_set_reset(port, 0, mask);

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
    let gpio = drv_stm32xx_sys_api::Sys::from(task);

    #[rustfmt::skip]
    ringbuf_entry!(
        Trace::GpioConfigure(port, mask, mode, output_type, speed, pull, af)
    );

    gpio.gpio_configure(port, mask, mode, output_type, speed, pull, af);

    Ok(0)
}

pub(crate) static HIFFY_FUNCS: &[Function] = &[
    crate::common::sleep,
    crate::common::send,
    crate::common::send_lease_read,
    crate::common::send_lease_read_write,
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
    #[cfg(feature = "spi")]
    spi_read,
    #[cfg(feature = "spi")]
    spi_write,
    #[cfg(feature = "qspi")]
    crate::common::qspi_read_id,
    #[cfg(feature = "qspi")]
    crate::common::qspi_read_status,
    #[cfg(feature = "qspi")]
    crate::common::qspi_bulk_erase,
    #[cfg(feature = "qspi")]
    crate::common::qspi_page_program,
    #[cfg(feature = "qspi")]
    crate::common::qspi_page_program_sector0,
    #[cfg(feature = "qspi")]
    crate::common::qspi_read,
    #[cfg(feature = "qspi")]
    crate::common::qspi_sector_erase,
    #[cfg(feature = "qspi")]
    crate::common::qspi_sector0_erase,
    #[cfg(feature = "qspi")]
    crate::common::qspi_verify,
    #[cfg(feature = "qspi")]
    crate::common::qspi_hash,
    #[cfg(feature = "hash")]
    hash_digest_sha256,
    #[cfg(feature = "hash")]
    hash_init_sha256,
    #[cfg(feature = "hash")]
    hash_update,
    #[cfg(feature = "hash")]
    hash_finalize_sha256,
];

//
// This definition forces the compiler to emit the DWARF needed for debuggers
// to be able to know function indices, arguments and return values.
//
#[no_mangle]
#[used]
static HIFFY_FUNCTIONS: Option<&Functions> = None;

pub(crate) fn trace_execute(offset: usize, op: hif::Op) {
    ringbuf_entry!(Trace::Execute((offset, op)));
}

pub(crate) fn trace_success() {
    ringbuf_entry!(Trace::Success);
}

pub(crate) fn trace_failure(f: hif::Failure) {
    ringbuf_entry!(Trace::Failure(f));
}
