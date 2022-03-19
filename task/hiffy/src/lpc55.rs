// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#[cfg(feature = "spi")]
use crate::common::{spi_read, spi_write};
use byteorder::ByteOrder;
use drv_lpc55_gpio_api::*;
use hif::*;
use hubris_num_tasks::Task;
use ringbuf::*;
use userlib::*;

#[cfg(feature = "gpio")]
task_slot!(GPIO, gpio_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Execute((usize, hif::Op)),
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
    Sleep(u16, u32),
    Send((Task, u16, Buffer, usize), u32),
    #[cfg(feature = "gpio")]
    GpioInput(drv_lpc55_gpio_api::Pin, drv_lpc55_gpio_api::GpioError),
    #[cfg(feature = "gpio")]
    GpioToggle(drv_lpc55_gpio_api::Pin, drv_lpc55_gpio_api::GpioError),
    #[cfg(feature = "gpio")]
    GpioSet(drv_lpc55_gpio_api::Pin, drv_lpc55_gpio_api::GpioError),
    #[cfg(feature = "gpio")]
    GpioReset(drv_lpc55_gpio_api::Pin, drv_lpc55_gpio_api::GpioError),
    #[cfg(feature = "gpio")]
    GpioConfigure(
        (
            drv_lpc55_gpio_api::Pin,
            drv_lpc55_gpio_api::AltFn,
            drv_lpc55_gpio_api::Mode,
            drv_lpc55_gpio_api::Slew,
            drv_lpc55_gpio_api::Invert,
            drv_lpc55_gpio_api::Digimode,
            drv_lpc55_gpio_api::Opendrain,
        ),
        drv_lpc55_gpio_api::GpioError,
    ),
    #[cfg(feature = "gpio")]
    GpioDirection(
        (drv_lpc55_gpio_api::Pin, drv_lpc55_gpio_api::Direction),
        drv_lpc55_gpio_api::GpioError,
    ),
    #[cfg(feature = "spi")]
    SpiRead((Task, u8, usize, usize), drv_spi_api::SpiError),
    #[cfg(feature = "spi")]
    SpiWrite((Task, u8, usize), drv_spi_api::SpiError),
}

#[cfg(feature = "gpio")]
fn gpio_args(
    stack: &[Option<u32>],
) -> Result<drv_lpc55_gpio_api::Pin, Failure> {
    if stack.len() < 1 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 1;

    let pin = match stack[fp + 0] {
        Some(pin) => match drv_lpc55_gpio_api::Pin::from_u32(pin) {
            Some(pin) => pin,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    Ok(pin)
}

#[cfg(feature = "gpio")]
fn gpio_configure(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.len() < 7 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 7;
    let pin = gpio_args(&stack[fp..fp + 1])?;

    let alt = match stack[fp + 1] {
        Some(alt) => match AltFn::from_u32(alt) {
            Some(alt) => alt,
            None => return Err(Failure::Fault(Fault::BadParameter(1))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(1))),
    };

    let mode = match stack[fp + 2] {
        Some(mode) => match Mode::from_u32(mode) {
            Some(mode) => mode,
            None => return Err(Failure::Fault(Fault::BadParameter(2))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(2))),
    };

    let slew = match stack[fp + 3] {
        Some(slew) => match Slew::from_u32(slew) {
            Some(slew) => slew,
            None => return Err(Failure::Fault(Fault::BadParameter(3))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(3))),
    };

    let invert = match stack[fp + 4] {
        Some(invert) => match Invert::from_u32(invert) {
            Some(invert) => invert,
            None => return Err(Failure::Fault(Fault::BadParameter(4))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(4))),
    };

    let digimode = match stack[fp + 5] {
        Some(digimode) => match Digimode::from_u32(digimode) {
            Some(digimode) => digimode,
            None => return Err(Failure::Fault(Fault::BadParameter(5))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(5))),
    };

    let opendrain = match stack[fp + 6] {
        Some(opendrain) => match Opendrain::from_u32(opendrain) {
            Some(opendrain) => opendrain,
            None => return Err(Failure::Fault(Fault::BadParameter(6))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(6))),
    };

    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Gpio::from(task);

    match gpio
        .iocon_configure(pin, alt, mode, slew, invert, digimode, opendrain)
    {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_toggle(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Gpio::from(task);

    let pin = gpio_args(stack)?;

    match gpio.toggle(pin) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_direction(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Gpio::from(task);

    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 2;
    let pin = gpio_args(&stack[fp..fp + 1])?;

    let dir = match stack[fp + 1] {
        Some(dir) => match Direction::from_u32(dir) {
            Some(dir) => dir,
            None => return Err(Failure::Fault(Fault::BadParameter(1))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(1))),
    };

    match gpio.set_dir(pin, dir) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_input(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Gpio::from(task);

    let pin = gpio_args(stack)?;

    match gpio.read_val(pin) {
        Ok(input) => {
            byteorder::LittleEndian::write_u16(rval, input as u16);
            Ok(core::mem::size_of::<u16>())
        }
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_set(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Gpio::from(task);

    let pin = gpio_args(stack)?;

    match gpio.set_val(pin, drv_lpc55_gpio_api::Value::One) {
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
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Gpio::from(task);

    let pin = gpio_args(stack)?;

    match gpio.set_val(pin, drv_lpc55_gpio_api::Value::Zero) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

pub(crate) static HIFFY_FUNCS: &[Function] = &[
    crate::common::sleep,
    crate::common::send,
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
    #[cfg(feature = "gpio")]
    gpio_direction,
    #[cfg(feature = "spi")]
    spi_read,
    #[cfg(feature = "spi")]
    spi_write,
];

//
// This definition forces the compiler to emit the DWARF needed for debuggers
// to be able to know function indices, arguments and return values.
//
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
