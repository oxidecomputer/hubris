// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Root of trust for reporting (RoT-R) task.
//!
//! Use the rotr-api crate to interact with this task.

#![no_std]
#![no_main]

use core::mem::MaybeUninit;
use dice::{AliasData, CertData, FwidCert};
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};
use ringbuf::{ringbuf, ringbuf_entry};
use stage0_handoff::HandoffData;
use task_attest_api::AttestError;
use userlib::UnwrapLite;
use zerocopy::AsBytes;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Cert,
    CertChainLen(u32),
    CertLen(usize),
    Record,
    Error(AttestError),
    Fwid([u8; MEASUREMENT_LEN]),
    Measurement([u8; MEASUREMENT_LEN]),
    BufSize(usize),
    Index(u32),
    Offset(usize),
    Startup,
    None,
}

ringbuf!(Trace, 16, Trace::None);

// Map the memory used to pass common DICE certificates to a variable.
#[used]
#[link_section = ".dice_certs"]
static CERTS: MaybeUninit<[u8; 0xa00]> = MaybeUninit::uninit();

// Map the memory used to pass artifacts intended for the attestation
// responder.
#[used]
#[link_section = ".dice_alias"]
static ALIAS: MaybeUninit<[u8; 0x800]> = MaybeUninit::uninit();

const MAX_MEASUREMENTS: usize = 16;
const MEASUREMENT_LEN: usize = 32;

struct AttestServer<'a> {
    alias_data: &'a AliasData,
    cert_data: &'a CertData,
    offset: usize,
    measurements: [[u8; MEASUREMENT_LEN]; MAX_MEASUREMENTS],
}

impl<'a> AttestServer<'a> {
    fn new(alias: &'a AliasData, certs: &'a CertData) -> Self {
        Self {
            alias_data: alias,
            cert_data: certs,
            offset: 0,
            measurements: [[0u8; MEASUREMENT_LEN]; MAX_MEASUREMENTS],
        }
    }

    fn get_cert_bytes_from_index(
        &self,
        index: u32,
    ) -> Result<&[u8], RequestError<AttestError>> {
        match index {
            // Cert chains start with the leaf and stop at the last
            // intermediate before the root. We mimic an array with
            // the leaf cert at index 0, and the last intermediate as
            // the chain length - 1.
            0 => Ok(self.alias_data.alias_cert.as_bytes()),
            1 => Ok(self.cert_data.deviceid_cert.as_bytes()),
            2 => Ok(&self.cert_data.persistid_cert.0.as_bytes()
                [0..self.cert_data.persistid_cert.0.size as usize]),
            3 => {
                let cert = self.cert_data.intermediate_cert.as_ref();
                if cert.is_some() {
                    let cert = cert.as_ref().unwrap_lite();
                    Ok(&cert.0.as_bytes()[0..cert.0.size as usize])
                } else {
                    Err(AttestError::IndexOverflow.into())
                }
            }
            _ => Err(AttestError::IndexOverflow.into()),
        }
    }
}

impl idl::InOrderAttestImpl for AttestServer<'_> {
    /// Receive and record a measurement of some downstream software artifact
    fn record(
        &mut self,
        _: &userlib::RecvMessage,
        hash_in: LenLimit<Leased<R, [u8]>, MEASUREMENT_LEN>,
    ) -> Result<(), RequestError<AttestError>> {
        ringbuf_entry!(Trace::Record);
        if hash_in.len() != MEASUREMENT_LEN {
            ringbuf_entry!(Trace::Error(AttestError::BadSize));
            return Err(AttestError::BadSize.into());
        }

        let mut hash = [0u8; MEASUREMENT_LEN];
        hash_in
            .read_range(0..MEASUREMENT_LEN, &mut hash)
            .map_err(|_| RequestError::went_away())?;

        if self.offset < MAX_MEASUREMENTS {
            ringbuf_entry!(Trace::Measurement(hash));

            self.measurements[self.offset] = hash;
            self.offset += 1;
        } else {
            ringbuf_entry!(Trace::Error(AttestError::LogFull));
            return Err(AttestError::LogFull.into());
        }

        Ok(())
    }

    /// Get length of cert chain from Alias to mfg intermediate
    fn cert_chain_len(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u32, RequestError<AttestError>> {
        // The cert chain will vary in length:
        // Stage0 w/ feature 'dice-self' will have 3 certs in the chain w/
        // the final cert being a self signed, puf derived identity key.
        // stage0 /w feature 'dice-mfg' will have 4 certs in the chain w/
        // the final ceert being the intermediate that links the identity
        // cert
        let chain_len = if self.cert_data.intermediate_cert.is_none() {
            3
        } else {
            4
        };

        ringbuf_entry!(Trace::CertChainLen(chain_len));
        Ok(chain_len)
    }

    /// Get length of cert at provided index in cert chain
    fn cert_len(
        &mut self,
        _: &userlib::RecvMessage,
        index: u32,
    ) -> Result<usize, RequestError<AttestError>> {
        let len = self.get_cert_bytes_from_index(index)?.len();

        ringbuf_entry!(Trace::CertLen(len));
        Ok(len)
    }

    /// Get a cert from the AliasCert chain
    fn cert(
        &mut self,
        _: &userlib::RecvMessage,
        index: u32,
        offset: usize,
        dest: Leased<W, [u8]>,
    ) -> Result<(), RequestError<AttestError>> {
        ringbuf_entry!(Trace::Cert);
        ringbuf_entry!(Trace::Index(index));
        ringbuf_entry!(Trace::Offset(offset));
        ringbuf_entry!(Trace::BufSize(dest.len()));

        let cert = self.get_cert_bytes_from_index(index)?;
        if cert.is_empty() {
            return Err(AttestError::IndexOverflow.into());
        }

        if offset + dest.len() > cert.len() {
            return Err(AttestError::Overflow.into());
        }

        dest.write_range(0..dest.len(), &cert[offset..offset + dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(())
    }
}

#[export_name = "main"]
fn main() -> ! {
    ringbuf_entry!(Trace::Startup);

    let addr = unsafe { CERTS.assume_init_ref() };
    let cert_data = match CertData::load_from_addr(addr) {
        Ok(a) => a,
        Err(_) => panic!("CertData"),
    };

    let addr = unsafe { ALIAS.assume_init_ref() };
    let alias_data = match AliasData::load_from_addr(addr) {
        Ok(a) => a,
        Err(_) => panic!("AliasData"),
    };

    let fwid = alias_data.alias_cert.get_fwid();
    ringbuf_entry!(Trace::Fwid(fwid.try_into().unwrap_lite()));

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut attest = AttestServer::new(&alias_data, &cert_data);
    loop {
        idol_runtime::dispatch(&mut buffer, &mut attest);
    }
}

mod idl {
    use super::AttestError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
