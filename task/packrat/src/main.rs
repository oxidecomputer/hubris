// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! VPD manipulation

#![no_std]
#![no_main]

use idol_runtime::RequestError;
use ringbuf::{ringbuf, ringbuf_entry};
use task_packrat_api::{
    CacheGetError, CacheSetError, MacAddressBlock, VpdIdentity,
};
use userlib::RecvMessage;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Trace {
    None,
    MacAddressBlockSet(TraceSet<MacAddressBlock>),
    VpdIdentitySet(TraceSet<VpdIdentity>),
}

impl From<TraceSet<MacAddressBlock>> for Trace {
    fn from(value: TraceSet<MacAddressBlock>) -> Self {
        Self::MacAddressBlockSet(value)
    }
}

impl From<TraceSet<VpdIdentity>> for Trace {
    fn from(value: TraceSet<VpdIdentity>) -> Self {
        Self::VpdIdentitySet(value)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum TraceSet<T> {
    // Initial set (always succeeds)
    Set(T),
    // Repeated set, but the same value as we have cached
    SetToSameValue(T),
    // Repeated set, but the value was different (returns an error to the
    // caller)
    AttemptedSetToNewValue(T),
}

ringbuf!(Trace, 16, Trace::None);

#[derive(Default)]
struct ServerImpl {
    mac_address_block: Option<MacAddressBlock>,
    identity: Option<VpdIdentity>,
}

impl ServerImpl {
    // Implementation for properties that may only be set once (e.g., our MAC
    // address block). If `storage` is already `Some(_)`, we log the extra set
    // and return an error if `value` doesn't match.
    fn set_once<T>(
        storage: &mut Option<T>,
        value: T,
    ) -> Result<(), CacheSetError>
    where
        Trace: From<TraceSet<T>>,
        T: PartialEq + Copy,
    {
        match storage {
            Some(prev) => {
                if *prev == value {
                    ringbuf_entry!(TraceSet::SetToSameValue(value).into());

                    // TODO Is this the right return value? Does a caller care
                    // if their set was actually ignored because we already had
                    // the value cached?
                    Ok(())
                } else {
                    ringbuf_entry!(
                        TraceSet::AttemptedSetToNewValue(value).into()
                    );
                    Err(CacheSetError::ValueAlreadySet)
                }
            }
            None => {
                ringbuf_entry!(TraceSet::Set(value).into());
                *storage = Some(value);
                Ok(())
            }
        }
    }
}

impl idl::InOrderPackratImpl for ServerImpl {
    fn get_mac_address_block(
        &mut self,
        _: &RecvMessage,
    ) -> Result<MacAddressBlock, RequestError<CacheGetError>> {
        let addrs = self.mac_address_block.ok_or(CacheGetError::ValueNotSet)?;
        Ok(addrs)
    }

    fn set_mac_address_block(
        &mut self,
        _: &RecvMessage,
        macs: MacAddressBlock,
    ) -> Result<(), RequestError<CacheSetError>> {
        Self::set_once(&mut self.mac_address_block, macs).map_err(Into::into)
    }

    fn get_identity(
        &mut self,
        _: &RecvMessage,
    ) -> Result<VpdIdentity, RequestError<CacheGetError>> {
        let addrs = self.identity.ok_or(CacheGetError::ValueNotSet)?;
        Ok(addrs)
    }

    fn set_identity(
        &mut self,
        _: &RecvMessage,
        identity: VpdIdentity,
    ) -> Result<(), RequestError<CacheSetError>> {
        Self::set_once(&mut self.identity, identity).map_err(Into::into)
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl::default();
    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{CacheGetError, CacheSetError, MacAddressBlock, VpdIdentity};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
