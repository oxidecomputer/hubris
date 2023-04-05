// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use hif::{Failure, Fault};
use hubris_num_tasks::NUM_TASKS;
#[allow(unused_imports)]
use userlib::task_slot;
#[cfg(any(feature = "sprot", feature = "update"))]
use userlib::FromPrimitive;
use userlib::{sys_refresh_task_id, sys_send, Generation, TaskId};
#[cfg(feature = "sprot")]
use zerocopy::AsBytes;

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
    if stack.is_empty() {
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

///
/// Function to send an arbitrary message to an arbitrary task.
///
/// arg2+n+1: Number of reply bytes
/// arg2+n: Number of bytes
/// arg2: Argument bytes
/// arg1: Operation
/// arg0: Task
///
#[allow(dead_code)]
pub(crate) fn send(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let mut payload = [0u8; 32];

    if stack.len() < 4 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let sp = stack.len();

    let nreply = match stack[sp - 1] {
        Some(nreply) => nreply as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(4)));
        }
    };

    if nreply > rval.len() {
        return Err(Failure::Fault(Fault::ReturnStackOverflow));
    }

    let nbytes = match stack[sp - 2] {
        Some(nbytes) => nbytes as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(3)));
        }
    };

    if stack.len() < nbytes + 4 {
        return Err(Failure::Fault(Fault::StackUnderflow));
    }

    let fp = sp - (nbytes + 4);

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

    let op = match stack[fp + 1] {
        Some(op) => {
            if op > core::u16::MAX.into() {
                return Err(Failure::Fault(Fault::BadParameter(1)));
            }

            op as u16
        }
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(1)));
        }
    };

    //
    // Time to assemble the actual bytes of our payload.
    //
    if nbytes > payload.len() {
        return Err(Failure::Fault(Fault::StackUnderflow));
    }

    let base = fp + 2;

    for i in base..base + nbytes {
        payload[i - base] = match stack[i] {
            Some(byte) => {
                if byte > core::u8::MAX.into() {
                    return Err(Failure::Fault(Fault::BadParameter(2)));
                }

                byte as u8
            }
            None => {
                return Err(Failure::Fault(Fault::EmptyParameter(2)));
            }
        };
    }

    //
    // We have it all! Time to send.
    //
    let (code, _) =
        sys_send(task, op, &payload[0..nbytes], &mut rval[0..nreply], &[]);

    if code != 0 {
        return Err(Failure::FunctionError(code));
    }

    Ok(nreply)
}

///
/// Function to send an arbitrary message to an arbitrary task with a single
/// read lease attached to `data`.
///
/// arg2+n+2: Size of lease
/// arg2+n+1: Number of reply bytes
/// arg2+n: Number of bytes
/// arg2: Argument bytes
/// arg1: Operation
/// arg0: Task
///
#[allow(dead_code)]
pub(crate) fn send_lease_read(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let mut payload = [0u8; 32];

    if stack.len() < 5 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let sp = stack.len();

    let nlease = match stack[sp - 1] {
        Some(nlease) => nlease as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(5)));
        }
    };
    if nlease > data.len() {
        return Err(Failure::Fault(Fault::BadParameter(5)));
    }

    let nreply = match stack[sp - 2] {
        Some(nreply) => nreply as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(4)));
        }
    };

    if nreply > rval.len() {
        return Err(Failure::Fault(Fault::ReturnStackOverflow));
    }

    let nbytes = match stack[sp - 3] {
        Some(nbytes) => nbytes as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(3)));
        }
    };

    if stack.len() < nbytes + 5 {
        return Err(Failure::Fault(Fault::StackUnderflow));
    }

    let fp = sp - (nbytes + 5);

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

    let op = match stack[fp + 1] {
        Some(op) => {
            if op > core::u16::MAX.into() {
                return Err(Failure::Fault(Fault::BadParameter(1)));
            }

            op as u16
        }
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(1)));
        }
    };

    //
    // Time to assemble the actual bytes of our payload.
    //
    if nbytes > payload.len() {
        return Err(Failure::Fault(Fault::StackUnderflow));
    }

    let base = fp + 2;

    for i in base..base + nbytes {
        payload[i - base] = match stack[i] {
            Some(byte) => {
                if byte > core::u8::MAX.into() {
                    return Err(Failure::Fault(Fault::BadParameter(2)));
                }

                byte as u8
            }
            None => {
                return Err(Failure::Fault(Fault::EmptyParameter(2)));
            }
        };
    }

    //
    // We have it all! Time to send.
    //
    let (code, _) = sys_send(
        task,
        op,
        &payload[0..nbytes],
        &mut rval[0..nreply],
        &[userlib::Lease::read_only(&data[..nlease])],
    );

    if code != 0 {
        return Err(Failure::FunctionError(code));
    }

    Ok(nreply)
}

///
/// Function to send an arbitrary message to an arbitrary task with a single
/// write lease attached to the tail end of `rval` (shared with reply bytes)
///
/// arg2+n+2: Size of lease
/// arg2+n+1: Number of reply bytes
/// arg2+n: Number of bytes
/// arg2: Argument bytes
/// arg1: Operation
/// arg0: Task
///
#[allow(dead_code)]
pub(crate) fn send_lease_write(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let mut payload = [0u8; 32];

    if stack.len() < 5 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let sp = stack.len();

    let nlease = match stack[sp - 1] {
        Some(nlease) => nlease as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(5)));
        }
    };

    let nreply = match stack[sp - 2] {
        Some(nreply) => nreply as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(4)));
        }
    };

    if nreply + nlease > rval.len() {
        return Err(Failure::Fault(Fault::ReturnStackOverflow));
    }

    let nbytes = match stack[sp - 3] {
        Some(nbytes) => nbytes as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(3)));
        }
    };

    if stack.len() < nbytes + 5 {
        return Err(Failure::Fault(Fault::StackUnderflow));
    }

    let fp = sp - (nbytes + 5);

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

    let op = match stack[fp + 1] {
        Some(op) => {
            if op > core::u16::MAX.into() {
                return Err(Failure::Fault(Fault::BadParameter(1)));
            }

            op as u16
        }
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(1)));
        }
    };

    //
    // Time to assemble the actual bytes of our payload.
    //
    if nbytes > payload.len() {
        return Err(Failure::Fault(Fault::StackUnderflow));
    }

    let base = fp + 2;

    for i in base..base + nbytes {
        payload[i - base] = match stack[i] {
            Some(byte) => {
                if byte > core::u8::MAX.into() {
                    return Err(Failure::Fault(Fault::BadParameter(2)));
                }

                byte as u8
            }
            None => {
                return Err(Failure::Fault(Fault::EmptyParameter(2)));
            }
        };
    }

    //
    // We have it all! Time to send.
    //
    let (rval, lease) = rval.split_at_mut(nreply);
    let (code, _) = sys_send(
        task,
        op,
        &payload[0..nbytes],
        rval,
        &[userlib::Lease::write_only(&mut lease[..nlease])],
    );

    if code != 0 {
        return Err(Failure::FunctionError(code));
    }

    Ok(nreply + nlease)
}

#[cfg(feature = "spi")]
fn spi_args(stack: &[Option<u32>]) -> Result<(TaskId, u8, usize), Failure> {
    if stack.len() < 3 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 3;

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

    let device = match stack[fp + 1] {
        Some(device) => device as u8,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(1)));
        }
    };

    let len = match stack[fp + 2] {
        Some(len) => len as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(2)));
        }
    };

    Ok((task, device, len))
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
    if stack.len() < 4 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 4;
    let (task, device, len) = spi_args(&stack[fp..fp + 3])?;

    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let rlen = match stack[fp + 3] {
        Some(rlen) => rlen as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(3)));
        }
    };

    if rlen > rval.len() {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }

    let spi = drv_spi_api::Spi::from(task);

    func_err(spi.exchange(device, &data[0..len], &mut rval[0..rlen]))?;
    Ok(rlen)
}

#[cfg(feature = "spi")]
pub(crate) fn spi_write(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let (task, device, len) = spi_args(stack)?;

    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let spi = drv_spi_api::Spi::from(task);

    func_err(spi.write(device, &data[0..len]))?;
    Ok(0)
}

/*
#[cfg(feature = "sprot")]
task_slot!(SPROT, sprot);

#[cfg(feature = "sprot")]
pub(crate) fn sprot_pulse_cs(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.is_empty() {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 1..];
    let delay: u16 =
        frame[0].ok_or(Failure::Fault(Fault::BadParameter(1)))? as u16;
    let server = drv_sprot_api::SpRot::from(SPROT.get_task_id());
    let pulse_status = func_err(server.pulse_cs(delay))?;
    let len = pulse_status.as_bytes().len();
    rval[..len].copy_from_slice(pulse_status.as_bytes());
    Ok(len)
}

#[cfg(feature = "sprot")]
pub(crate) fn sprot_write_block(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let (start_block, len) = update_args(stack)?;

    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let sprot = drv_sprot_api::SpRot::from(SPROT.get_task_id());

    let block_size = func_err(sprot.block_size())?;

    for (i, c) in data[..len].chunks(block_size).enumerate() {
        func_err(sprot.write_one_block((start_block + i) as u32, c))?;
    }
    Ok(0)
}

#[cfg(feature = "sprot")]
pub(crate) fn sprot_start_update(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.is_empty() {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 1;

    let target = match stack[fp + 0] {
        Some(target) => target as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(0)));
        }
    };

    let img = match drv_update_api::UpdateTarget::from_usize(target) {
        Some(i) => i,
        None => return Err(Failure::Fault(Fault::BadParameter(0))),
    };

    func_err(
        drv_sprot_api::SpRot::from(SPROT.get_task_id()).prep_image_update(img),
    )?;
    Ok(0)
}

#[cfg(feature = "sprot")]
pub(crate) fn sprot_finish_update(
    _stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    func_err(
        drv_sprot_api::SpRot::from(SPROT.get_task_id()).finish_image_update(),
    )?;
    Ok(0)
}

#[cfg(feature = "sprot")]
pub(crate) fn sprot_block_size(
    _stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let size =
        func_err(drv_sprot_api::SpRot::from(SPROT.get_task_id()).block_size())?;

    let bytes: [u8; 4] = [
        (size & 0xff) as u8,
        ((size >> 8) & 0xff) as u8,
        ((size >> 16) & 0xff) as u8,
        ((size >> 24) & 0xff) as u8,
    ];

    rval[..4].copy_from_slice(&bytes);
    Ok(4)
}

#[cfg(feature = "sprot")]
pub(crate) fn sprot_switch_default_image(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let (slot, duration) = switch_default_image_args(stack)?;
    func_err(
        drv_sprot_api::SpRot::from(SPROT.get_task_id())
            .switch_default_image(slot, duration),
    )?;
    Ok(0)
}

#[cfg(feature = "sprot")]
pub(crate) fn sprot_reset(
    _stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    func_err(drv_sprot_api::SpRot::from(SPROT.get_task_id()).reset())?;
    Ok(0)
}
*/

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

    if rval.is_empty() {
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
    func_err(
        server.bulk_erase(hf::HfProtectMode::AllowModificationsToSector0),
    )?;
    Ok(0)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_page_program(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    qspi_page_program_inner(
        stack,
        data,
        drv_gimlet_hf_api::HfProtectMode::ProtectSector0,
    )
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_page_program_sector0(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    qspi_page_program_inner(
        stack,
        data,
        drv_gimlet_hf_api::HfProtectMode::AllowModificationsToSector0,
    )
}

#[cfg(feature = "qspi")]
fn qspi_page_program_inner(
    stack: &[Option<u32>],
    data: &[u8],
    protect: drv_gimlet_hf_api::HfProtectMode,
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
    func_err(server.page_program(addr, protect, data))?;
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

    if stack.is_empty() {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 1..];
    let addr = frame[0].ok_or(Failure::Fault(Fault::MissingParameters))?;

    let server = hf::HostFlash::from(HF.get_task_id());
    func_err(server.sector_erase(addr, hf::HfProtectMode::ProtectSector0))?;
    Ok(0)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_sector0_erase(
    _stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;

    let server = hf::HostFlash::from(HF.get_task_id());
    func_err(
        server.sector_erase(0, hf::HfProtectMode::AllowModificationsToSector0),
    )?;
    Ok(0)
}

#[cfg(feature = "hash")]
task_slot!(HASH, hash_driver);

// TODO: port this
#[cfg(all(feature = "qspi", feature = "hash"))]
pub(crate) fn qspi_hash(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_gimlet_hf_api as hf;
    use drv_hash_api as hash;

    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 2..];
    let addr = frame[0].ok_or(Failure::Fault(Fault::MissingParameters))?;
    let len = frame[1].ok_or(Failure::Fault(Fault::MissingParameters))?;

    if rval.len() < hash::SHA256_SZ {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }

    let server = hf::HostFlash::from(HF.get_task_id());
    let sha256sum = func_err(server.hash(addr, len))?;
    rval[..hash::SHA256_SZ].copy_from_slice(&sha256sum);
    Ok(hash::SHA256_SZ)
}

#[cfg(feature = "hash")]
pub(crate) fn hash_init_sha256(
    _stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_hash_api as hash;

    let server = hash::Hash::from(HASH.get_task_id());
    func_err(server.init_sha256())?;
    Ok(0)
}

#[cfg(feature = "hash")]
pub(crate) fn hash_digest_sha256(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_hash_api as hash;

    if rval.len() < hash::SHA256_SZ {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }

    if stack.is_empty() {
        // return Err(Failure::Fault(Fault::MissingParameters));
        return Err(Failure::Fault(Fault::BadParameter(0)));
    }
    let frame = &stack[stack.len() - 1..];
    let len = frame[0].ok_or(Failure::Fault(Fault::BadParameter(1)))? as usize;
    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let server = hash::Hash::from(HASH.get_task_id());
    let sha256sum = func_err(server.digest_sha256(len as u32, &data[0..len]))?;
    rval[..hash::SHA256_SZ].copy_from_slice(&sha256sum);
    Ok(hash::SHA256_SZ)
}

#[cfg(feature = "hash")]
pub(crate) fn hash_update(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_hash_api as hash;

    if stack.is_empty() {
        return Err(Failure::Fault(Fault::BadParameter(0)));
    }
    let frame = &stack[stack.len() - 1..];
    let len = frame[0].ok_or(Failure::Fault(Fault::BadParameter(1)))? as usize;
    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let server = hash::Hash::from(HASH.get_task_id());
    func_err(server.update(len as u32, &data[..len]))?;
    Ok(0)
}

#[cfg(feature = "hash")]
pub(crate) fn hash_finalize_sha256(
    _stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_hash_api as hash;

    if rval.len() < hash::SHA256_SZ {
        // XXX use a well defined constant
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }
    let server = hash::Hash::from(HASH.get_task_id());
    let sha256sum = func_err(server.finalize_sha256())?;
    rval[..hash::SHA256_SZ].copy_from_slice(&sha256sum);
    Ok(hash::SHA256_SZ)
}

#[cfg(feature = "rng")]
task_slot!(RNG, rng_driver);

#[cfg(feature = "rng")]
pub(crate) fn rng_fill(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_rng_api::Rng;

    if stack.is_empty() {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    if stack.len() > 1 {
        return Err(Failure::Fault(Fault::BadParameter(2)));
    }

    let frame = &stack[stack.len() - 1..];
    let count =
        frame[0].ok_or(Failure::Fault(Fault::MissingParameters))? as usize;
    if count > rval.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    func_err(Rng::from(RNG.get_task_id()).fill(&mut rval[0..count]))?;
    Ok(count)
}

#[cfg(feature = "update")]
task_slot!(UPDATE, update_server);

#[cfg(any(feature = "update", feature = "sprot"))]
fn update_args(stack: &[Option<u32>]) -> Result<(usize, usize), Failure> {
    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 2;

    let len = match stack[fp + 0] {
        Some(len) => len as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(0)));
        }
    };

    let block_num = match stack[fp + 1] {
        Some(len) => len as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(0)));
        }
    };

    Ok((block_num, len))
}

#[cfg(feature = "update")]
pub(crate) fn write_block(
    stack: &[Option<u32>],
    data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let (start_block, len) = update_args(stack)?;

    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let update = drv_update_api::Update::from(UPDATE.get_task_id());

    let block_size = func_err(update.block_size())?;

    for (i, c) in data[..len].chunks(block_size).enumerate() {
        func_err(update.write_one_block(start_block + i, c))?;
    }

    Ok(0)
}

#[cfg(feature = "update")]
pub(crate) fn start_update(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.is_empty() {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 1;

    let target = match stack[fp + 0] {
        Some(target) => target as usize,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(0)));
        }
    };

    let img = match drv_update_api::UpdateTarget::from_usize(target) {
        Some(i) => i,
        None => return Err(Failure::Fault(Fault::BadParameter(0))),
    };

    func_err(
        drv_update_api::Update::from(UPDATE.get_task_id())
            .prep_image_update(img),
    )?;
    Ok(0)
}

#[cfg(feature = "update")]
pub(crate) fn finish_update(
    _stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    func_err(
        drv_update_api::Update::from(UPDATE.get_task_id())
            .finish_image_update(),
    )?;
    Ok(0)
}

#[cfg(feature = "update")]
pub(crate) fn block_size(
    _stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let size = func_err(
        drv_update_api::Update::from(UPDATE.get_task_id()).block_size(),
    )?;

    let bytes: [u8; 4] = [
        (size & 0xff) as u8,
        ((size >> 8) & 0xff) as u8,
        ((size >> 16) & 0xff) as u8,
        ((size >> 24) & 0xff) as u8,
    ];

    rval[..4].copy_from_slice(&bytes);

    Ok(4)
}

#[cfg(any(feature = "sprot", feature = "update"))]
fn switch_default_image_args(
    stack: &[Option<u32>],
) -> Result<(drv_update_api::SlotId, drv_update_api::SwitchDuration), Failure> {
    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let fp = stack.len() - 2;
    let slot: drv_update_api::SlotId = match stack[fp + 0] {
        Some(slot) => match drv_update_api::SlotId::from_u8(slot as u8) {
            Some(slot) => slot,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };
    let duration: drv_update_api::SwitchDuration = match stack[fp + 1] {
        Some(duration) => {
            match drv_update_api::SwitchDuration::from_u32(duration) {
                Some(target) => target,
                None => return Err(Failure::Fault(Fault::BadParameter(1))),
            }
        }
        None => return Err(Failure::Fault(Fault::EmptyParameter(1))),
    };
    Ok((slot, duration))
}

#[cfg(feature = "update")]
pub(crate) fn switch_default_image(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let (slot, duration) = switch_default_image_args(stack)?;

    func_err(
        drv_update_api::Update::from(UPDATE.get_task_id())
            .switch_default_image(slot, duration),
    )?;
    Ok(0)
}

#[cfg(feature = "update")]
pub(crate) fn reset(
    _stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    func_err(drv_update_api::Update::from(UPDATE.get_task_id()).reset())?;
    Ok(0)
}
