cfg_if::cfg_if! {
    if #[cfg(feature = "spi")] {
        use hif::{Fault, Failure};
        use userlib::{sys_refresh_task_id, Generation, TaskId, NUM_TASKS};
    }
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
    match spi.exchange(0, &data[0..len], &mut rval[0..rlen]) {
        Ok(_) => Ok(rlen),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
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
    match spi.write(0, &data[0..len]) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "qspi")]
use userlib::*;

#[cfg(feature = "qspi")]
declare_task!(HF, hf);

#[cfg(feature = "qspi")]
pub(crate) fn qspi_read_id(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    if rval.len() < 20 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }

    let (rc, len) = userlib::sys_send(
        userlib::get_task_id(HF),
        1,
        &[],
        &mut rval[..20],
        &[],
    );
    if len != 20 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }
    if rc != 0 {
        return Err(Failure::FunctionError(rc));
    }

    Ok(20)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_read_status(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    if rval.len() < 1 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }

    let (rc, len) = userlib::sys_send(
        userlib::get_task_id(HF),
        2,
        &[],
        &mut rval[..1],
        &[],
    );
    if len != 1 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }
    if rc != 0 {
        return Err(Failure::FunctionError(rc));
    }

    Ok(1)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_bulk_erase(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    let (rc, len) =
        userlib::sys_send(userlib::get_task_id(HF), 3, &[], &mut [], &[]);
    if len != 0 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }
    if rc != 0 {
        return Err(Failure::FunctionError(rc));
    }

    Ok(0)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_page_program(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use zerocopy::AsBytes;

    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 2..];
    let addr = frame[0].ok_or(Failure::Fault(Fault::MissingParameters))?;
    let len =
        frame[1].ok_or(Failure::Fault(Fault::MissingParameters))? as usize;

    if len > data.len() {
        return Err(Failure::Fault(Fault::AccessOutOfBounds));
    }

    let data = &data[..len];

    let (rc, len) = userlib::sys_send(
        userlib::get_task_id(HF),
        4,
        addr.as_bytes(),
        &mut [],
        &[Lease::from(data)],
    );
    if len != 0 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }
    if rc != 0 {
        return Err(Failure::FunctionError(rc));
    }

    Ok(0)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_read(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use zerocopy::AsBytes;

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

    let (rc, rlen) = userlib::sys_send(
        userlib::get_task_id(HF),
        5,
        addr.as_bytes(),
        &mut [],
        &[Lease::from(out)],
    );
    if rlen != 0 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }
    if rc != 0 {
        return Err(Failure::FunctionError(rc));
    }

    Ok(len)
}

#[cfg(feature = "qspi")]
pub(crate) fn qspi_sector_erase(
    stack: &[Option<u32>],
    data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use zerocopy::AsBytes;

    if stack.len() < 1 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }
    let frame = &stack[stack.len() - 1..];
    let addr = frame[0].ok_or(Failure::Fault(Fault::MissingParameters))?;

    let (rc, len) = userlib::sys_send(
        userlib::get_task_id(HF),
        6,
        addr.as_bytes(),
        &mut [],
        &[],
    );
    if len != 0 {
        return Err(Failure::Fault(Fault::ReturnValueOverflow));
    }
    if rc != 0 {
        return Err(Failure::FunctionError(rc));
    }

    Ok(0)
}
