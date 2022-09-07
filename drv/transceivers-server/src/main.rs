// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_sidecar_front_io::transceivers::Transceivers;
use drv_transceivers_api::{ModulesStatus, TransceiversError};
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
        Ok(ModulesStatus::from(
            self.transceivers
                .get_modules_status()
                .map_err(TransceiversError::from)?,
        ))
    }

    fn set_power_enable(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        Ok(self
            .transceivers
            .set_power_enable(mask)
            .map_err(TransceiversError::from)?)
    }

    fn clear_power_enable(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        Ok(self
            .transceivers
            .clear_power_enable(mask)
            .map_err(TransceiversError::from)?)
    }

    fn set_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        Ok(self
            .transceivers
            .set_reset(mask)
            .map_err(TransceiversError::from)?)
    }

    fn clear_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        Ok(self
            .transceivers
            .clear_reset(mask)
            .map_err(TransceiversError::from)?)
    }

    fn setup_i2c_op(
        &mut self,
        _msg: &userlib::RecvMessage,
        is_read: bool,
        reg: u8,
        num_bytes: u8,
        mask: u32,
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        Ok(self
            .transceivers
            .setup_i2c_op(is_read, reg, num_bytes, mask)
            .map_err(TransceiversError::from)?)
    }

    fn get_i2c_read_buffer(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
        num_bytes: u8,
    ) -> Result<[u8; 128], idol_runtime::RequestError<TransceiversError>> {
        let mut buf: [u8; 128] = [0; 128];
        self.transceivers
            .get_i2c_read_buffer(port, &mut buf[..(num_bytes as usize)])
            .map_err(TransceiversError::from)?;

        Ok(buf)
    }

    fn set_i2c_write_buffer(
        &mut self,
        _msg: &userlib::RecvMessage,
        num_bytes: u8,
        buf: [u8; 128],
    ) -> Result<(), idol_runtime::RequestError<TransceiversError>> {
        Ok(self
            .transceivers
            .set_i2c_write_buffer(&buf[..(num_bytes as usize)])
            .map_err(TransceiversError::from)?)
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
