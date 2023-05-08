// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Gimlet Apcb query server.
//!
//! This server is responsible for managing access to the Apcb; it talks
//! to the gimlet hf server.

#![no_std]
#![no_main]

use userlib::*;
use amd_apcb::{Apcb, ApcbIoOptions};
use drv_gimlet_hf_api as hf_api;
use drv_gimlet_hf_api::SECTOR_SIZE_BYTES;
use drv_gimlet_apcb_api::{ApcbError};
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};
use zerocopy::{AsBytes, FromBytes};

//task_slot!(SYS, sys);
task_slot!(HF, hf);

#[export_name = "main"]
fn main() -> ! {
    //let sys = sys_api::Sys::from(SYS.get_task_id());
    let hf = hf_api::HostFlash::from(HF.get_task_id());

    //sys.gpio_set(cfg.reset);
    hl::sleep_for(10);

    let mut server = ServerImpl {
        hf,
    };

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
/*
    fn get_persistent_data(&mut self) -> Result<HfPersistentData, HfError> {
        let out = self.get_raw_persistent_data()?;
        Ok(HfPersistentData {
            dev_select: HfDevSelect::from_u8(out.dev_select as u8).unwrap(),
        })
    }
*/
}

impl idl::InOrderApcbImpl for ServerImpl {
    fn apcb_token_value(
        &mut self,
        msg: &userlib::RecvMessage,
        entry_id: u32,
        token_id: u32,
     ) -> Result<u32, idol_runtime::RequestError<ApcbError>>
     where ApcbError: idol_runtime::IHaveConsideredServerDeathWithThisErrorType {
        let mut buffer = [0u8; 1000];
        let apcb = Apcb::load(&mut buffer[..], &ApcbIoOptions::default());
        Ok(42) // FIXME
     }
}

mod idl {
    use super::{
        //HfDevSelect,
        ApcbError,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
