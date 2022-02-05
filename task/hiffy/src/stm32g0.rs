// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use hif::*;
#[cfg(any(feature = "gpio"))]
use userlib::*;

#[cfg(feature = "gpio")]
task_slot!(SYS, sys);

pub struct Buffer(u8);

//
// The order in this enum must match the order in the functions array that
// is passed to execute.
//
pub enum Functions {
    Sleep(u16, u32),
    #[cfg(feature = "gpio")]
    GpioInput(drv_stm32xx_sys_api::Port, drv_stm32xx_sys_api::GpioError),
    #[cfg(feature = "gpio")]
    GpioToggle(
        (drv_stm32xx_sys_api::Port, u8),
        drv_stm32xx_sys_api::GpioError,
    ),
    #[cfg(feature = "gpio")]
    GpioSet(
        (drv_stm32xx_sys_api::Port, u8),
        drv_stm32xx_sys_api::GpioError,
    ),
    #[cfg(feature = "gpio")]
    GpioReset(
        (drv_stm32xx_sys_api::Port, u8),
        drv_stm32xx_sys_api::GpioError,
    ),
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
        drv_stm32xx_sys_api::GpioError,
    ),
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
        Some(pin) if pin < 16 => (1u16 << pin),
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

    if stack.len() < 1 {
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

    match sys.gpio_read_input(port) {
        Ok(input) => {
            byteorder::LittleEndian::write_u16(rval, input);
            Ok(core::mem::size_of::<u16>())
        }
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
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
        Err(err) => Err(Failure::FunctionError(err.into())),
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

    match sys.gpio_set_reset(port, mask, 0) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
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

    match sys.gpio_set_reset(port, 0, mask) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
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

    match sys.gpio_configure(port, mask, mode, output_type, speed, pull, af) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

pub(crate) static HIFFY_FUNCS: &[Function] = &[
    crate::common::sleep,
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
static HIFFY_FUNCTIONS: Option<&Functions> = None;

pub(crate) fn trace_execute(_offset: usize, _op: hif::Op) {}

pub(crate) fn trace_success() {}

pub(crate) fn trace_failure(_f: hif::Failure) {}
