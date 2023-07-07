// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! packrat: a task for caching data.
//!
//! There are several cases where we want a task to always start with the same
//! data; e.g., once `net` has come online and chosen a MAC address, it should
//! always use that MAC address even if the `net` task restarts. Packrat solves
//! this problem by being a place where tasks can store data (in the MAC address
//! case, the sequencer task for the relevant board reads the VPD and populates
//! the MAC address in packrat) that can be read back by any task (e.g., `net`).
//!
//! It is critical that packrat itself never restart, as a restart would cause
//! packrat to lose all the data it should be remembering! We attempt to
//! accomplish this via simplicity:
//!
//! 1. All of packrat's functionality should be straightforward get/set
//!    operations in memory; it makes no hardware accesses. For data that
//!    packrat should have that comes from hardware access (e.g., VPD), some
//!    other task is responsible for accessing hardware and then sending data to
//!    packrat.
//! 2. packrat does no parsing of incoming or outgoing data, other than that
//!    generated by the idol server implementation. It should call no fallible
//!    functions.
//! 3. packrat never calls into any other task, as calling into a task gives the
//!    callee opportunity to fault the caller.

#![no_std]
#![no_main]

use core::convert::Infallible;
use idol_runtime::{Leased, LenLimit, RequestError};
use mutable_statics::mutable_statics;
use ringbuf::{ringbuf, ringbuf_entry};
use task_packrat_api::{
    CacheGetError, CacheSetError, HostStartupOptions, MacAddressBlock,
    VpdIdentity,
};
use userlib::RecvMessage;

#[cfg(feature = "gimlet")]
mod gimlet;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // not all variants are used, depending on cargo features
enum Trace {
    None,
    MacAddressBlockSet(TraceSet<MacAddressBlock>),
    VpdIdentitySet(TraceSet<VpdIdentity>),
    SetNextBootHostStartupOptions(HostStartupOptions),
    SpdDataUpdate {
        index: u8,
        page1: bool,
        offset: u8,
        len: u8,
    },
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

#[export_name = "main"]
fn main() -> ! {
    let (mac_address_block, identity) = mutable_statics! {
        static mut MAC_ADDRESS_BLOCK: [Option<MacAddressBlock>; 1]
            = [|| None; _];
        static mut IDENTITY: [Option<VpdIdentity>; 1] = [|| None; _];
    };

    let mut server = ServerImpl {
        mac_address_block: &mut mac_address_block[0],
        identity: &mut identity[0],
        #[cfg(feature = "gimlet")]
        gimlet_data: gimlet::GimletData::claim_static_resources(),
    };

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    mac_address_block: &'static mut Option<MacAddressBlock>,
    identity: &'static mut Option<VpdIdentity>,
    #[cfg(feature = "gimlet")]
    gimlet_data: gimlet::GimletData,
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

    #[cfg(feature = "gimlet")]
    fn get_next_boot_host_startup_options(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HostStartupOptions, RequestError<Infallible>> {
        Ok(self.gimlet_data.host_startup_options())
    }

    #[cfg(not(feature = "gimlet"))]
    fn get_next_boot_host_startup_options(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HostStartupOptions, RequestError<Infallible>> {
        Err(RequestError::Fail(
            idol_runtime::ClientError::BadMessageContents,
        ))
    }

    #[cfg(feature = "gimlet")]
    fn set_next_boot_host_startup_options(
        &mut self,
        _: &RecvMessage,
        host_startup_options: HostStartupOptions,
    ) -> Result<(), RequestError<Infallible>> {
        ringbuf_entry!(Trace::SetNextBootHostStartupOptions(
            host_startup_options
        ));
        self.gimlet_data
            .set_host_startup_options(host_startup_options);
        Ok(())
    }

    #[cfg(not(feature = "gimlet"))]
    fn set_next_boot_host_startup_options(
        &mut self,
        _: &RecvMessage,
        _host_startup_options: HostStartupOptions,
    ) -> Result<(), RequestError<Infallible>> {
        Err(RequestError::Fail(
            idol_runtime::ClientError::BadMessageContents,
        ))
    }

    #[cfg(feature = "gimlet")]
    fn set_spd_eeprom(
        &mut self,
        _: &RecvMessage,
        index: u8,
        page1: bool,
        offset: u8,
        data: LenLimit<Leased<idol_runtime::R, [u8]>, 256>,
    ) -> Result<(), RequestError<Infallible>> {
        self.gimlet_data.set_spd_eeprom(index, page1, offset, data)
    }

    #[cfg(not(feature = "gimlet"))]
    fn set_spd_eeprom(
        &mut self,
        _: &RecvMessage,
        _index: u8,
        _page1: bool,
        _offset: u8,
        _data: LenLimit<Leased<idol_runtime::R, [u8]>, 256>,
    ) -> Result<(), RequestError<Infallible>> {
        Err(RequestError::Fail(
            idol_runtime::ClientError::BadMessageContents,
        ))
    }

    #[cfg(feature = "gimlet")]
    fn get_spd_present(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<bool, RequestError<Infallible>> {
        self.gimlet_data.get_spd_present(index)
    }

    #[cfg(not(feature = "gimlet"))]
    fn get_spd_present(
        &mut self,
        _: &RecvMessage,
        _index: usize,
    ) -> Result<bool, RequestError<Infallible>> {
        Err(RequestError::Fail(
            idol_runtime::ClientError::BadMessageContents,
        ))
    }

    #[cfg(feature = "gimlet")]
    fn get_spd_data(
        &mut self,
        _: &RecvMessage,
        index: usize,
    ) -> Result<u8, RequestError<Infallible>> {
        self.gimlet_data.get_spd_data(index)
    }

    #[cfg(not(feature = "gimlet"))]
    fn get_spd_data(
        &mut self,
        _: &RecvMessage,
        _index: usize,
    ) -> Result<u8, RequestError<Infallible>> {
        Err(RequestError::Fail(
            idol_runtime::ClientError::BadMessageContents,
        ))
    }

    #[cfg(feature = "gimlet")]
    fn get_full_spd_data(
        &mut self,
        _: &RecvMessage,
        dev: usize,
        out: LenLimit<Leased<idol_runtime::W, [u8]>, 512>,
    ) -> Result<(), RequestError<Infallible>> {
        self.gimlet_data.get_full_spd_data(dev, out)
    }

    #[cfg(not(feature = "gimlet"))]
    fn get_full_spd_data(
        &mut self,
        _: &RecvMessage,
        _dev: usize,
        _out: LenLimit<Leased<idol_runtime::W, [u8]>, 512>,
    ) -> Result<u8, RequestError<Infallible>> {
        Err(RequestError::Fail(
            idol_runtime::ClientError::BadMessageContents,
        ))
    }
}

mod idl {
    use super::{
        CacheGetError, CacheSetError, HostStartupOptions, MacAddressBlock,
        VpdIdentity,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
