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
#[cfg(feature = "spctrl")]
task_slot!(SP_CTRL, swd);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Execute((usize, hif::Op)),
    Failure(Failure),
    Success,
    None,
}

ringbuf!(Trace, 64, Trace::None);

// TODO: this type is copy-pasted in several modules
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
    #[cfg(feature = "gpio")]
    GpioInput(drv_lpc55_gpio_api::Pin, u32),
    #[cfg(feature = "gpio")]
    GpioToggle(drv_lpc55_gpio_api::Pin, u32),
    #[cfg(feature = "gpio")]
    GpioSet(drv_lpc55_gpio_api::Pin, u32),
    #[cfg(feature = "gpio")]
    GpioReset(drv_lpc55_gpio_api::Pin, u32),
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
        u32,
    ),
    #[cfg(feature = "gpio")]
    GpioDirection(
        (drv_lpc55_gpio_api::Pin, drv_lpc55_gpio_api::Direction),
        u32,
    ),
    #[cfg(feature = "spi")]
    SpiRead((Task, u8, usize, usize), drv_spi_api::SpiError),
    #[cfg(feature = "spi")]
    SpiWrite((Task, u8, usize), drv_spi_api::SpiError),
    #[cfg(feature = "spctrl")]
    WriteToSp((u32, u32), drv_sp_ctrl_api::SpCtrlError),
    #[cfg(feature = "spctrl")]
    ReadFromSp((u32, u32), drv_sp_ctrl_api::SpCtrlError),
    #[cfg(feature = "spctrl")]
    SpCtrlInit((), drv_sp_ctrl_api::SpCtrlError),
    #[cfg(feature = "spctrl")]
    DbResetSp((), drv_sp_ctrl_api::SpCtrlError),
}

#[cfg(feature = "spctrl")]
pub(crate) fn sp_ctrl_init(
    _stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = SP_CTRL.get_task_id();
    let sp_ctrl = drv_sp_ctrl_api::SpCtrl::from(task);

    match sp_ctrl.setup() {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "spctrl")]
fn sp_ctrl_args(stack: &[Option<u32>]) -> Result<(u32, usize), Failure> {
    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 2;

    let addr = match stack[fp + 0] {
        Some(addr) => addr,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(0)));
        }
    };

    let len = match stack[fp + 1] {
        Some(len) => len as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(1)));
        }
    };

    Ok((addr, len))
}

#[cfg(feature = "spctrl")]
pub(crate) fn write_to_sp(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let (addr, len) = sp_ctrl_args(stack)?;

    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let task = SP_CTRL.get_task_id();
    let sp_ctrl = drv_sp_ctrl_api::SpCtrl::from(task);

    match sp_ctrl.write(addr, &data[0..len]) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "spctrl")]
pub(crate) fn read_from_sp(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let (addr, len) = sp_ctrl_args(stack)?;

    if len > rval.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let task = SP_CTRL.get_task_id();
    let sp_ctrl = drv_sp_ctrl_api::SpCtrl::from(task);

    match sp_ctrl.read(addr, &mut rval[0..len]) {
        Ok(_) => Ok(len),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "spctrl")]
pub(crate) fn db_reset_sp(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.is_empty() {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let fp = stack.len() - 1;
    let delay = match stack[fp + 0] {
        Some(delay) => delay,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(0)));
        }
    };

    let task = SP_CTRL.get_task_id();
    let sp_ctrl = drv_sp_ctrl_api::SpCtrl::from(task);

    sp_ctrl.db_reset_sp(delay);
    Ok(0)
}

#[cfg(feature = "gpio")]
fn gpio_args(
    stack: &[Option<u32>],
) -> Result<drv_lpc55_gpio_api::Pin, Failure> {
    if stack.is_empty() {
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
    let gpio = drv_lpc55_gpio_api::Pins::from(task);

    gpio.iocon_configure(
        pin, alt, mode, slew, invert, digimode, opendrain, None,
    );

    Ok(0)
}

#[cfg(feature = "gpio")]
fn gpio_toggle(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Pins::from(task);

    let pin = gpio_args(stack)?;

    match gpio.toggle(pin) {
        Ok(_) => Ok(0),
        Err(idol_runtime::ServerDeath) => panic!(),
    }
}

#[cfg(feature = "gpio")]
fn gpio_direction(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Pins::from(task);

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

    gpio.set_dir(pin, dir);

    Ok(0)
}

#[cfg(feature = "gpio")]
fn gpio_input(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Pins::from(task);

    let pin = gpio_args(stack)?;

    let input = gpio.read_val(pin);

    byteorder::LittleEndian::write_u16(rval, input as u16);
    Ok(core::mem::size_of::<u16>())
}

#[cfg(feature = "gpio")]
fn gpio_set(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Pins::from(task);

    let pin = gpio_args(stack)?;

    gpio.set_val(pin, drv_lpc55_gpio_api::Value::One);

    Ok(0)
}

#[cfg(feature = "gpio")]
fn gpio_reset(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = GPIO.get_task_id();
    let gpio = drv_lpc55_gpio_api::Pins::from(task);

    let pin = gpio_args(stack)?;

    gpio.set_val(pin, drv_lpc55_gpio_api::Value::Zero);

    Ok(0)
}

pub(crate) static HIFFY_FUNCS: &[Function] = &[
    crate::common::sleep,
    crate::common::send,
    crate::common::send_lease_read,
    crate::common::send_lease_read_write,
    crate::common::send_lease_write,
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
    #[cfg(feature = "spctrl")]
    write_to_sp,
    #[cfg(feature = "spctrl")]
    read_from_sp,
    #[cfg(feature = "spctrl")]
    sp_ctrl_init,
    #[cfg(feature = "spctrl")]
    db_reset_sp,
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
