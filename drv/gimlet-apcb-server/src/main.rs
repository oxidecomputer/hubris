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
use amd_efs::{
    BhdDirectoryEntry, BhdDirectoryEntryType, DirectoryEntry, Efs,
    ProcessorGeneration,
};
use amd_flash::{
    ErasableLocation, FlashAlign, FlashRead, FlashWrite, Location,
};
use drv_gimlet_apcb_api::ApcbError;
use drv_gimlet_hf_api as hf_api;
use drv_gimlet_hf_api::SECTOR_SIZE_BYTES;
use userlib::*;

//task_slot!(SYS, sys);
task_slot!(HF, hf);

#[export_name = "main"]
fn main() -> ! {
    //let sys = sys_api::Sys::from(SYS.get_task_id());
    let hf = hf_api::HostFlash::from(HF.get_task_id());

    //sys.gpio_set(cfg.reset);
    //hl::sleep_for(10);

    let mut server = ServerImpl {
        storage: Storage::new(hf),
    };

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

struct Storage {
    hf: hf_api::HostFlash,
}

impl Storage {
    fn new(hf: hf_api::HostFlash) -> Self {
        Self { hf }
    }
}

impl amd_flash::FlashRead for Storage {
    fn read_exact(
        &self,
        location: Location,
        buffer: &mut [u8],
    ) -> amd_flash::Result<()> {
        self.hf.read(location, buffer).unwrap(); // FIXME: Make apcb address not fixed
        Ok(())
    }
}

impl FlashAlign for Storage {
    fn erasable_block_size(&self) -> usize {
        SECTOR_SIZE_BYTES // or something?
    }
}

impl FlashWrite for Storage {
    fn erase_block(
        &self,
        _location: ErasableLocation,
    ) -> amd_flash::Result<()> {
        Err(amd_flash::Error::Programmer)
    }
    fn erase_and_write_block(
        &self,
        _location: ErasableLocation,
        _buffer: &[u8],
    ) -> amd_flash::Result<()> {
        Err(amd_flash::Error::Programmer)
    }
}

struct ServerImpl {
    storage: Storage,
}

impl ServerImpl {}

impl idl::InOrderApcbImpl for ServerImpl {
    fn apcb_token_value(
        &mut self,
        _msg: &userlib::RecvMessage,
        instance_id: u16,
        entry_id: u16,
        token_id: u32,
    ) -> Result<u32, idol_runtime::RequestError<ApcbError>>
    where
        ApcbError: idol_runtime::IHaveConsideredServerDeathWithThisErrorType,
    {
        let mut buffer = [0xFFu8; Apcb::MAX_SIZE];
        //let capacity = self.hf.capacity().unwrap();

        let processor_generation = ProcessorGeneration::Milan;
        let efs = Efs::<Storage>::load(
            &self.storage,
            Some(processor_generation),
            None,
        )
        .unwrap();
        let bhd_directory =
            efs.bhd_directory(Some(processor_generation)).unwrap();
        for entry in bhd_directory.entries() {
            if let Ok(typ) = entry.typ_or_err() {
                if typ == BhdDirectoryEntryType::ApcbBackup
                    && entry.sub_program() == 1
                    && entry.instance() == 0
                {
                    let payload_beginning =
                        bhd_directory.payload_beginning(&entry).unwrap();
                    let size = entry.size().unwrap() as usize;
                    self.storage
                        .read_exact(payload_beginning, &mut buffer[0..size])
                        .unwrap();

                    let apcb =
                        Apcb::load(&mut buffer[..], &ApcbIoOptions::default())
                            .map_err(|_| idol_runtime::RequestError::<ApcbError>::from(ApcbError::FIXME))?;
                    let tokens = apcb
                        .tokens(instance_id, BoardInstances::new())
                        .map_err(|_| idol_runtime::RequestError::<ApcbError>::from(ApcbError::FIXME))?;
                    let value = tokens
                        .get(
                            TokenEntryId::from_u16(entry_id)
                                .ok_or(idol_runtime::RequestError::<ApcbError>::from(ApcbError::FIXME))?,
                            token_id,
                        )
                        .map_err(|_| idol_runtime::RequestError::<ApcbError>::from(ApcbError::FIXME))?;
                    return Ok(value);
                }
            }
        }
        Err(ApcbError::FIXME.into())
    }
}

mod idl {
    use super::ApcbError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
