// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Gimlet Apcb query server.
//!
//! This server is responsible for managing access to the Apcb; it talks
//! to the gimlet hf server.

#![no_std]
#![no_main]

use amd_apcb::{Apcb, ApcbIoOptions, BoardInstances, TokenEntryId};
use drv_gimlet_apcb_api::ApcbError;
use drv_gimlet_hf_api as hf_api;
use drv_gimlet_hf_api::SECTOR_SIZE_BYTES;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};
use userlib::*;
use zerocopy::{AsBytes, FromBytes};

//task_slot!(SYS, sys);
task_slot!(HF, hf);

#[export_name = "main"]
fn main() -> ! {
    //let sys = sys_api::Sys::from(SYS.get_task_id());
    let hf = hf_api::HostFlash::from(HF.get_task_id());

    //sys.gpio_set(cfg.reset);
    hl::sleep_for(10);

    let mut server = ServerImpl { hf };

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl {
    hf: hf_api::HostFlash,
}

impl ServerImpl {
}

impl idl::InOrderApcbImpl for ServerImpl {
    fn apcb_token_value(
        &mut self,
        msg: &userlib::RecvMessage,
        instance_id: u16,
        entry_id: u16,
        token_id: u32,
    ) -> Result<u32, idol_runtime::RequestError<ApcbError>>
    where
        ApcbError: idol_runtime::IHaveConsideredServerDeathWithThisErrorType,
    {
        let mut buffer = [0u8; 1000];
        let apcb = Apcb::load(&mut buffer[..], &ApcbIoOptions::default())
            .map_err(|_| ApcbError::FIXME)?;
        let tokens = apcb
            .tokens(instance_id, BoardInstances::new())
            .map_err(|_| ApcbError::FIXME)?;
        let value = tokens
            .get(
                TokenEntryId::from_u16(entry_id).ok_or(ApcbError::FIXME)?,
                token_id,
            )
            .map_err(|_| ApcbError::FIXME)?;
        Ok(value)
    }
}

mod idl {
    use super::ApcbError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
