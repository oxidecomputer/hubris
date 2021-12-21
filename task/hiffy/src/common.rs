// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use hif::{Failure, Fault};

cfg_if::cfg_if! {
    if #[cfg(feature = "spi")] {
        use userlib::{sys_refresh_task_id, Generation, TaskId};
        use hubris_num_tasks::NUM_TASKS;
    }
}

/// We allow dead code on this because the functions below are optional.
///
/// This could become a From impl on Failure if moved into hif, which would let
/// it be replaced syntactically by a question mark.
#[allow(dead_code)]
fn func_err<T, E>(e: Result<T, E>) -> Result<T, hif::Failure>
where
    E: Into<u32>,
{
    e.map_err(|e| hif::Failure::FunctionError(e.into()))
}

///
/// Function to sleep(), which takes a single parameter: the number of
/// milliseconds.  This is expected to be short:  if sleeping for any
/// serious length of time, it should be done on the initiator side, not
/// on the Hubris side.  (The purpose of this function is to allow for
/// device-mandated sleeps to in turn for allow for bulk device operations.)
///
pub(crate) fn sleep(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.len() < 1 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 1;
    let ms = match stack[fp] {
        Some(ms) if ms > 0 && ms <= 100 => Ok(ms),
        _ => Err(Failure::Fault(Fault::BadParameter(0))),
    }?;

    userlib::hl::sleep_for(ms.into());

    Ok(0)
}

#[cfg(feature = "spi")]
fn spi_args(stack: &[Option<u32>]) -> Result<(TaskId, usize), Failure> {
    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 2;

    let task = match stack[fp + 0] {
        Some(task) => {
            if task >= NUM_TASKS as u32 {
                return Err(Failure::Fault(Fault::BadParameter(0)));
            }

            let prototype =
                TaskId::for_index_and_gen(task as usize, Generation::default());

            sys_refresh_task_id(prototype)
        }
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

    Ok((task, len))
}

#[cfg(feature = "spi")]
pub(crate) fn spi_read(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    //
    // We have our task ID, our write size, and our read size
    //
    if stack.len() < 3 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 3;
    let (task, len) = spi_args(&stack[fp..fp + 2])?;

    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let rlen = match stack[fp + 2] {
        Some(rlen) => rlen as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(2)));
        }
    };

    if rlen > rval.len() {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }

    let spi = drv_spi_api::Spi::from(task);

    // TODO: hiffy currently always issues SPI commands to device 0. It is worth
    // changing this at some point.
    func_err(spi.exchange(0, &data[0..len], &mut rval[0..rlen]))?;
    Ok(rlen)
}

#[cfg(feature = "spi")]
pub(crate) fn spi_write(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let (task, len) = spi_args(stack)?;

    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let spi = drv_spi_api::Spi::from(task);

    // TODO: hiffy currently always issues SPI commands to device 0. It is worth
    // changing this at some point.
    func_err(spi.write(0, &data[0..len]))?;
    Ok(0)
}

#[cfg(feature = "qspi")]
use userlib::*;

#[cfg(feature = "qspi")]
task_slot!(HF, hf);

#[cfg(feature = "qspi")]
pub(crate) fn qspi_read_id(
    _stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;

    if rval.len() < 20 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }

    let server = hf::HostFlash::from(HF.get_task_id());
    let id = func_err(server.read_id())?;
    rval[..20].copy_from_slice(&id);
    Ok(20)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_read_status(
    _stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;

    if rval.len() < 1 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }

    let server = hf::HostFlash::from(HF.get_task_id());
    let x = func_err(server.read_status())?;
    rval[0] = x;
    Ok(1)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_bulk_erase(
    _stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;

    let server = hf::HostFlash::from(HF.get_task_id());
    func_err(server.bulk_erase())?;
    Ok(0)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_page_program(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;

    if stack.len() < 3 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 3..];
    let addr = frame[0].ok_or(Failure::Fault(Fault::MissingParameters))?;
    let offset =
        frame[1].ok_or(Failure::Fault(Fault::MissingParameters))? as usize;
    let len =
        frame[2].ok_or(Failure::Fault(Fault::MissingParameters))? as usize;

    if offset + len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let data = &data[offset..offset + len];

    let server = hf::HostFlash::from(HF.get_task_id());
    func_err(server.page_program(addr, data))?;
    Ok(0)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_read(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;

    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 2..];
    let addr = frame[0].ok_or(Failure::Fault(Fault::MissingParameters))?;
    let len =
        frame[1].ok_or(Failure::Fault(Fault::MissingParameters))? as usize;

    if len > rval.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let out = &mut rval[..len];

    let server = hf::HostFlash::from(HF.get_task_id());
    func_err(server.read(addr, out))?;
    Ok(len)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_verify(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;

    if stack.len() < 3 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 3..];
    let addr = frame[0].ok_or(Failure::Fault(Fault::MissingParameters))?;
    let offset =
        frame[1].ok_or(Failure::Fault(Fault::MissingParameters))? as usize;
    let len =
        frame[2].ok_or(Failure::Fault(Fault::MissingParameters))? as usize;

    if offset + len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    if len > rval.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let data = &data[offset..offset + len];
    let out = &mut rval[..len];

    let server = hf::HostFlash::from(HF.get_task_id());
    func_err(server.read(addr, out))?;

    let mut differ = false;

    for i in 0..len {
        if data[i] != out[i] {
            differ = true;
            break;
        }
    }

    if differ {
        rval[0] = 1;
    } else {
        rval[0] = 0;
    }

    Ok(1)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_sector_erase(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;

    if stack.len() < 1 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 1..];
    let addr = frame[0].ok_or(Failure::Fault(Fault::MissingParameters))?;

    let server = hf::HostFlash::from(HF.get_task_id());
    func_err(server.sector_erase(addr))?;
    Ok(0)
}
