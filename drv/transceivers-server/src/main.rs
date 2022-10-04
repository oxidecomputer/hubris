// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_sidecar_front_io::transceivers::Transceivers;
use drv_transceivers_api::{
    ModulesStatus, TransceiversError, NUM_PORTS, PAGE_SIZE_BYTES,
};
use idol_runtime::{ClientError, Leased, RequestError, R, W};
use userlib::task_slot;

task_slot!(FRONT_IO, front_io);

struct ServerImpl {
    transceivers: Transceivers,
}

impl idl::InOrderTransceiversImpl for ServerImpl {
    fn get_modules_status(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ModulesStatus, idol_runtime::RequestError<TransceiversError>>
    {
        Ok(self
            .transceivers
            .get_modules_status()
            .map_err(TransceiversError::from)?)
    }

    fn set_power_enable(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .set_power_enable(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn clear_power_enable(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .clear_power_enable(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn set_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .set_reset(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn clear_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .clear_reset(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn set_lpmode(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .set_lpmode(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn clear_lpmode(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        self.transceivers
            .clear_lpmode(mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn setup_i2c_op(
        &mut self,
        _msg: &userlib::RecvMessage,
        is_read: bool,
        reg: u8,
        num_bytes: u8,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if usize::from(num_bytes) > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        self.transceivers
            .setup_i2c_op(is_read, reg, num_bytes, mask)
            .map_err(TransceiversError::from)?;
        Ok(())
    }

    fn get_i2c_read_buffer(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
        dest: Leased<W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if port >= NUM_PORTS {
            return Err(TransceiversError::InvalidPortNumber.into());
        }

        if dest.len() > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        let mut buf = [0u8; PAGE_SIZE_BYTES];

        self.transceivers
            .get_i2c_read_buffer(port, &mut buf[..dest.len()])
            .map_err(TransceiversError::from)?;

        dest.write_range(0..dest.len(), &buf[..dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        Ok(())
    }

    fn set_i2c_write_buffer(
        &mut self,
        _msg: &userlib::RecvMessage,
        data: Leased<R, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        if data.len() > PAGE_SIZE_BYTES {
            return Err(TransceiversError::InvalidNumberOfBytes.into());
        }

        let mut buf = [0u8; PAGE_SIZE_BYTES];

        data.read_range(0..data.len(), &mut buf[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        self.transceivers
            .set_i2c_write_buffer(&buf[..data.len()])
            .map_err(TransceiversError::from)?;
        Ok(())
    }
}

#[export_name = "main"]
fn main() -> ! {
    loop {
        let mut buffer = [0; idl::INCOMING_SIZE];
        let transceivers = Transceivers::new(FRONT_IO.get_task_id());

        let mut server = ServerImpl { transceivers };

        loop {
            idol_runtime::dispatch(&mut buffer, &mut server);
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::{ModulesStatus, TransceiversError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
