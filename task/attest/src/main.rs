// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Root of trust for reporting (RoT-R) task.
//!
//! Use the attest-api crate to interact with this task.

#![no_std]
#![no_main]

mod config;

use attest_api::{AttestError, HashAlgorithm, NONCE_MAX_SIZE, NONCE_MIN_SIZE};
use attest_data::{
    Attestation, Ed25519Signature, Log, Measurement, Sha3_256Digest,
};
use config::DataRegion;
use core::slice;
use hubpack::SerializedSize;
use idol_runtime::{
    ClientError, Leased, LenLimit, NotificationHandler, RequestError, R, W,
};
use lib_dice::{AliasData, CertData, SeedBuf};
use ringbuf::{ringbuf, ringbuf_entry};
use salty::signature::Keypair;
use serde::Deserialize;
use sha3::{Digest as CryptDigest, Sha3_256};
use stage0_handoff::{HandoffData, HandoffDataLoadError};
use zerocopy::AsBytes;

// This file is generated by the crate build.rs. It contains instances of
// config::DataRegion structs describing regions of memory configured &
// exposed to this task by the hubris build.
mod build {
    include!(concat!(env!("OUT_DIR"), "/attest-config.rs"));
}

use build::{ALIAS_DATA, CERT_DATA};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Cert,
    CertChainLen(u32),
    CertLen(usize),
    AttestError(AttestError),
    HandoffError(HandoffDataLoadError),
    BufSize(usize),
    Index(u32),
    Offset(u32),
    Startup,
    Record(HashAlgorithm),
    BadLease(usize),
    LogLen(u32),
    Log,
    ClientError(ClientError),
    None,
}

ringbuf!(Trace, 16, Trace::None);

/// Load a type implementing HandoffData (and others) from a config::DataRegion.
/// Errors will be reported in the ringbuf and will return in None.
fn load_data_from_region<
    T: for<'a> Deserialize<'a> + HandoffData + SerializedSize,
>(
    region: &DataRegion,
) -> Option<T> {
    // Safety: This memory is setup by code executed before hubris and
    // exposed using the kernel `extern-regions` mechanism. The safety of
    // this code is an extension of our trust in the hubris kernel / build.
    let data = unsafe {
        slice::from_raw_parts(region.address as *mut u8, region.size as usize)
    };

    match T::load_from_addr(data) {
        Ok(d) => Some(d),
        Err(e) => {
            ringbuf_entry!(Trace::HandoffError(e));
            None
        }
    }
}

struct AttestServer {
    alias_data: Option<AliasData>,
    alias_keypair: Option<Keypair>,
    buf: &'static mut [u8; Log::MAX_SIZE],
    cert_data: Option<CertData>,
    measurements: Log,
}

impl Default for AttestServer {
    fn default() -> Self {
        static LOG_BUF: ClaimOnceCell<[u8; Log::MAX_SIZE]> =
            ClaimOnceCell::new([0; Log::MAX_SIZE]);

        let alias_data: Option<AliasData> = load_data_from_region(&ALIAS_DATA);
        let alias_keypair = alias_data
            .as_ref()
            .map(|d| Keypair::from(d.alias_seed.as_bytes()));

        Self {
            alias_data,
            alias_keypair,
            buf: LOG_BUF.claim(),
            cert_data: load_data_from_region(&CERT_DATA),
            measurements: Log::default(),
        }
    }
}

impl AttestServer {
    fn get_cert_bytes_from_index(
        &self,
        index: u32,
    ) -> Result<&[u8], RequestError<AttestError>> {
        let alias_data =
            self.alias_data.as_ref().ok_or(AttestError::NoCerts)?;
        let cert_data = self.cert_data.as_ref().ok_or(AttestError::NoCerts)?;

        match index {
            // Cert chains start with the leaf and stop at the last
            // intermediate before the root. We mimic an array with
            // the leaf cert at index 0, and the last intermediate as
            // the chain length - 1.
            0 => Ok(alias_data.alias_cert.as_bytes()),
            1 => Ok(cert_data.deviceid_cert.as_bytes()),
            2 => Ok(&cert_data.persistid_cert.0.as_bytes()
                [0..cert_data.persistid_cert.0.size as usize]),
            3 => {
                if let Some(cert) = cert_data.intermediate_cert.as_ref() {
                    Ok(&cert.0.as_bytes()[0..cert.0.size as usize])
                } else {
                    Err(AttestError::InvalidCertIndex.into())
                }
            }
            _ => Err(AttestError::InvalidCertIndex.into()),
        }
    }
}

impl idl::InOrderAttestImpl for AttestServer {
    /// Get length of cert chain from Alias to mfg intermediate
    fn cert_chain_len(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u32, RequestError<AttestError>> {
        let cert_data = self.cert_data.as_ref().ok_or(AttestError::NoCerts)?;
        // The cert chain will vary in length:
        // - kernel w/ feature 'dice-self' will have 3 certs in the chain w/
        // the final cert being a self signed, puf derived identity key
        // - kernel /w feature 'dice-mfg' will have 4 certs in the chain w/
        // the final cert being the intermediate that signs the identity
        // cert
        let chain_len = if cert_data.intermediate_cert.is_none() {
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
    ) -> Result<u32, RequestError<AttestError>> {
        let len = self.get_cert_bytes_from_index(index)?.len();
        ringbuf_entry!(Trace::CertLen(len));

        let len = u32::try_from(len).map_err(|_| AttestError::CertTooBig)?;

        Ok(len)
    }

    /// Get a cert from the AliasCert chain
    fn cert(
        &mut self,
        _: &userlib::RecvMessage,
        index: u32,
        offset: u32,
        dest: Leased<W, [u8]>,
    ) -> Result<(), RequestError<AttestError>> {
        ringbuf_entry!(Trace::Cert);
        ringbuf_entry!(Trace::Index(index));
        ringbuf_entry!(Trace::Offset(offset));
        ringbuf_entry!(Trace::BufSize(dest.len()));

        let cert = self.get_cert_bytes_from_index(index)?;
        if cert.is_empty() {
            let err = AttestError::InvalidCertIndex;
            ringbuf_entry!(Trace::AttestError(err));
            return Err(err.into());
        }

        let offset = offset as usize;
        // the offset provided must not exceed the length of the cert & there
        // must be sufficient data from the offset to the end of the cert to
        // fill the lease
        if cert.len() < offset || dest.len() > cert.len() - offset {
            let err = AttestError::OutOfRange;
            ringbuf_entry!(Trace::AttestError(err));
            return Err(err.into());
        }

        dest.write_range(0..dest.len(), &cert[offset..offset + dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(())
    }

    fn record(
        &mut self,
        _: &userlib::RecvMessage,
        algorithm: HashAlgorithm,
        data: idol_runtime::Leased<idol_runtime::R, [u8]>,
    ) -> Result<(), RequestError<AttestError>> {
        ringbuf_entry!(Trace::Record(algorithm));

        if self.measurements.is_full() {
            return Err(AttestError::LogFull.into());
        }

        let measurement = match algorithm {
            HashAlgorithm::Sha3_256 => {
                if data.len() != Sha3_256Digest::LENGTH {
                    ringbuf_entry!(Trace::BadLease(data.len()));
                    return Err(AttestError::BadLease.into());
                }

                let mut digest = Sha3_256Digest::default();
                data.read_range(0..digest.0.len(), &mut digest.0)
                    .map_err(|_| RequestError::went_away())?;

                Measurement::Sha3_256(digest)
            }
        };

        self.measurements.push(measurement);

        Ok(())
    }

    fn log(
        &mut self,
        _: &userlib::RecvMessage,
        offset: u32,
        dest: Leased<W, [u8]>,
    ) -> Result<(), RequestError<AttestError>> {
        ringbuf_entry!(Trace::Log);

        let offset = offset as usize;
        let log_len = hubpack::serialize(self.buf, &self.measurements)
            .map_err(|_| AttestError::SerializeLog)?;

        if log_len < offset || dest.len() > log_len - offset {
            let err = AttestError::OutOfRange;
            ringbuf_entry!(Trace::AttestError(err));
            return Err(err.into());
        }

        dest.write_range(0..dest.len(), &self.buf[offset..offset + dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(())
    }

    fn log_len(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u32, RequestError<AttestError>> {
        let len = hubpack::serialize(self.buf, &self.measurements)
            .map_err(|_| AttestError::SerializeLog)?;
        let len = u32::try_from(len).map_err(|_| AttestError::LogTooBig)?;

        ringbuf_entry!(Trace::LogLen(len));

        Ok(len)
    }

    fn attest(
        &mut self,
        _: &userlib::RecvMessage,
        nonce: LenLimit<Leased<R, [u8]>, { NONCE_MAX_SIZE }>,
        dest: Leased<W, [u8]>,
    ) -> Result<(), RequestError<AttestError>> {
        if nonce.len() < NONCE_MIN_SIZE {
            let err = AttestError::BadLease;
            ringbuf_entry!(Trace::AttestError(err));
            return Err(err.into());
        }

        // serialize measurement log
        let len =
            hubpack::serialize(self.buf, &self.measurements).map_err(|_| {
                let e = AttestError::SerializeLog;
                ringbuf_entry!(Trace::AttestError(e));
                e
            })?;
        let _ = u32::try_from(len).map_err(|_| {
            let e = AttestError::LogTooBig;
            ringbuf_entry!(Trace::AttestError(e));
            e
        })?;

        // sha3_256(hubpack(measurement_log) | nonce)
        let mut digest = Sha3_256::new();
        digest.update(&self.buf[..len]);

        let len = nonce.len();
        let mut nonce_bytes = [0u8; NONCE_MAX_SIZE];
        nonce
            .read_range(0..len, &mut nonce_bytes[..len])
            .map_err(|_| {
                let e = ClientError::WentAway;
                ringbuf_entry!(Trace::ClientError(e));
                RequestError::Fail(e)
            })?;

        let nonce = nonce_bytes;
        digest.update(&nonce[..len]);

        // get key pair used to generate signatures / attestations
        // NOTE: replace `map_err` w/ `inspect_err` when it's stable
        let alias_keypair = self
            .alias_keypair
            .as_ref()
            .ok_or(AttestError::NoCerts)
            .map_err(|e| {
                ringbuf_entry!(Trace::AttestError(e));
                e
            })?;

        // generate attestation:
        // sign(alias_priv, sha3_256(hubpack(measurement_log) | nonce)
        let digest = digest.finalize();
        let signature = alias_keypair.sign(&digest);
        let signature =
            Attestation::Ed25519(Ed25519Signature::from(signature.to_bytes()));

        // serialize / hubpack attestation into temp buffer
        let len = hubpack::serialize(self.buf, &signature).map_err(|_| {
            let e = AttestError::SerializeSignature;
            ringbuf_entry!(Trace::AttestError(e));
            e
        })?;

        if dest.len() != len {
            let err = AttestError::BadLease;
            ringbuf_entry!(Trace::AttestError(err));
            return Err(err.into());
        }

        // copy attestation from temp buffer to output lease
        dest.write_range(0..dest.len(), &self.buf[0..len])
            .map_err(|_| {
                let e = ClientError::WentAway;
                ringbuf_entry!(Trace::ClientError(e));
                RequestError::Fail(e)
            })
    }

    fn attest_len(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u32, RequestError<AttestError>> {
        // this may become inaccurate when additional variants are added to
        // `enum Attestation`
        let len = u32::try_from(Attestation::MAX_SIZE).map_err(|_| {
            let e = AttestError::SignatureTooBig;
            ringbuf_entry!(Trace::AttestError(e));
            e
        })?;

        Ok(len)
    }
}

impl NotificationHandler for AttestServer {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    ringbuf_entry!(Trace::Startup);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut attest = AttestServer::default();
    loop {
        idol_runtime::dispatch(&mut buffer, &mut attest);
    }
}

mod idl {
    use super::{AttestError, HashAlgorithm};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
