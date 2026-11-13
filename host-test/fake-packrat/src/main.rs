// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Fake packrat for host tests.
//! robo:
//! robo: Accepts VPD (MAC block and identity) and records every ereport it's
//! robo: handed, for the orchestrator to read back through
//! robo: [`host_test_api::packrat`]. Every other operation faults the caller.

use core::convert::Infallible;

use gateway_ereport_messages as ereport_messages;
use host_test_api::control::{Control, serve_one};
use host_test_api::packrat::{Ereport, Request, Response};
use idol_runtime::{ClientError, Leased, LenLimit, RequestError};
use task_packrat_api::{
    CacheGetError, CacheSetError, EreportReadError, EreportWriteError,
    HostBootfailReadOutput, HostInfoReadError, HostInfoRequest,
    HostInfoWriteOutput, HostPanicReadOutput, HostStartupOptions,
    MacAddressBlock, OxideIdentity,
};
use userlib::{RecvMessage, sys_get_timer};

/// robo: Most ereports returned per control reply.
const EREPORTS_PER_REPLY: usize = 32;

#[derive(Default)]
struct ServerImpl {
    macs: Option<MacAddressBlock>,
    identity: Option<OxideIdentity>,
    ereports: Vec<Ereport>,
}

fn unsupported<T, E>() -> Result<T, RequestError<E>> {
    Err(ClientError::UnknownOperation.fail())
}

impl idl::InOrderPackratImpl for ServerImpl {
    fn get_mac_address_block(
        &mut self,
        _: &RecvMessage,
    ) -> Result<MacAddressBlock, RequestError<CacheGetError>> {
        self.macs.ok_or(CacheGetError::ValueNotSet.into())
    }

    fn set_mac_address_block(
        &mut self,
        _: &RecvMessage,
        macs: MacAddressBlock,
    ) -> Result<(), RequestError<CacheSetError>> {
        if self.macs.is_some() {
            return Err(CacheSetError::ValueAlreadySet.into());
        }
        self.macs = Some(macs);
        Ok(())
    }

    fn get_identity(
        &mut self,
        _: &RecvMessage,
    ) -> Result<OxideIdentity, RequestError<CacheGetError>> {
        self.identity.ok_or(CacheGetError::ValueNotSet.into())
    }

    fn set_identity(
        &mut self,
        _: &RecvMessage,
        identity: OxideIdentity,
    ) -> Result<(), RequestError<CacheSetError>> {
        if self.identity.is_some() {
            return Err(CacheSetError::ValueAlreadySet.into());
        }
        self.identity = Some(identity);
        Ok(())
    }

    fn get_next_boot_host_startup_options(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HostStartupOptions, RequestError<Infallible>> {
        unsupported()
    }

    fn set_next_boot_host_startup_options(
        &mut self,
        _: &RecvMessage,
        _: HostStartupOptions,
    ) -> Result<(), RequestError<Infallible>> {
        unsupported()
    }

    fn set_spd_eeprom(
        &mut self,
        _: &RecvMessage,
        _: u8,
        _: usize,
        _: LenLimit<Leased<idol_runtime::R, [u8]>, 256>,
    ) -> Result<(), RequestError<Infallible>> {
        unsupported()
    }

    fn remove_spd(
        &mut self,
        _: &RecvMessage,
        _: u8,
    ) -> Result<(), RequestError<Infallible>> {
        unsupported()
    }

    fn get_spd_present(
        &mut self,
        _: &RecvMessage,
        _: u8,
    ) -> Result<bool, RequestError<Infallible>> {
        unsupported()
    }

    fn get_spd_data(
        &mut self,
        _: &RecvMessage,
        _: u8,
        _: usize,
    ) -> Result<u8, RequestError<Infallible>> {
        unsupported()
    }

    fn get_full_spd_data(
        &mut self,
        _: &RecvMessage,
        _: u8,
        _: Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), RequestError<Infallible>> {
        unsupported()
    }

    fn set_restart_id(
        &mut self,
        _: &RecvMessage,
        _: u128,
    ) -> Result<(), RequestError<CacheSetError>> {
        unsupported()
    }

    fn deliver_encoded_ereport(
        &mut self,
        msg: &RecvMessage,
        data: LenLimit<Leased<idol_runtime::R, [u8]>, 1024usize>,
    ) -> Result<ereport_messages::Ena, RequestError<EreportWriteError>> {
        let data = data.into_inner();
        let mut bytes = vec![0u8; data.len()];
        data.read_range(0..data.len(), &mut bytes)
            .map_err(|()| RequestError::Fail(ClientError::WentAway))?;
        self.ereports.push(Ereport {
            time: sys_get_timer().now,
            task: msg.sender.index() as u16,
            bytes,
        });
        Ok(ereport_messages::Ena((self.ereports.len() as u64).into()))
    }

    fn read_ereports(
        &mut self,
        _: &RecvMessage,
        _: ereport_messages::RequestIdV0,
        _: ereport_messages::RestartId,
        _: ereport_messages::Ena,
        _: u8,
        _: ereport_messages::Ena,
        _: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<EreportReadError>> {
        unsupported()
    }

    fn write_host_bootfail(
        &mut self,
        _: &RecvMessage,
        _: u8,
        _: Option<u16>,
        _: Leased<idol_runtime::R, [u8]>,
    ) -> Result<HostInfoWriteOutput, RequestError<Infallible>> {
        unsupported()
    }

    fn read_host_bootfail_fragment(
        &mut self,
        _: &RecvMessage,
        _: Option<HostInfoRequest>,
        _: Leased<idol_runtime::W, [u8]>,
    ) -> Result<HostBootfailReadOutput, RequestError<HostInfoReadError>> {
        unsupported()
    }

    fn write_host_panic(
        &mut self,
        _: &RecvMessage,
        _: Option<u16>,
        _: Leased<idol_runtime::R, [u8]>,
    ) -> Result<HostInfoWriteOutput, RequestError<Infallible>> {
        unsupported()
    }

    fn read_host_panic_fragment(
        &mut self,
        _: &RecvMessage,
        _: Option<HostInfoRequest>,
        _: Leased<idol_runtime::W, [u8]>,
    ) -> Result<HostPanicReadOutput, RequestError<HostInfoReadError>> {
        unsupported()
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

impl Control for ServerImpl {
    type Request = Request;
    type Response = Response;

    fn control(&mut self, req: Request) -> Response {
        match req {
            Request::Ereports { from } => Response::Ereports(
                self.ereports
                    .iter()
                    .skip(from)
                    .take(EREPORTS_PER_REPLY)
                    .cloned()
                    .collect(),
            ),
        }
    }
}

/// robo: Receive buffer size: fits any operation of the real API, and
/// robo: control requests.
const BUFFER_SIZE: usize = if idl::INCOMING_SIZE > 1024 {
    idl::INCOMING_SIZE
} else {
    1024
};

fn main() -> ! {
    let mut server = ServerImpl::default();
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        serve_one::<_, idl::PackratOperation>(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{
        CacheGetError, CacheSetError, EreportReadError, EreportWriteError,
        HostBootfailReadOutput, HostInfoReadError, HostInfoRequest,
        HostInfoWriteOutput, HostPanicReadOutput, HostStartupOptions,
        MacAddressBlock, OxideIdentity, ereport_messages,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
