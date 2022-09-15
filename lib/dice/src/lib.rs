// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![cfg_attr(not(test), no_std)]

use core::{mem, slice, str::FromStr};
use hkdf::Hkdf;
use hubpack::SerializedSize;
use salty::constants::SECRETKEY_SEED_LENGTH;
use serde::{Deserialize, Serialize};
use sha3::Sha3_256;
use zerocopy::AsBytes;
use zeroize::{Zeroize, ZeroizeOnDrop};

mod cert;
pub use crate::cert::{AliasCert, Cert, CertError, DeviceIdSelfCert};
mod alias_cert_tmpl;
mod deviceid_cert_tmpl;
mod handoff;
pub use crate::handoff::{AliasData, Handoff};

pub const SEED_LENGTH: usize = SECRETKEY_SEED_LENGTH;
// We define the length of the serial number using the values from the alias
// cert template though it's consistent across all templates.
pub const SN_LENGTH: usize = alias_cert_tmpl::ISSUER_SN_RANGE.end
    - alias_cert_tmpl::ISSUER_SN_RANGE.start;
const REG_ADDR_NONSEC: u32 = 0x40000900;

fn get_cdi_reg_slice() -> &'static mut [u32] {
    // SAFETY: Dereferencing this raw pointer is necessary to read the CDI
    // from the LPC55 DICE registers. This pointer will always reference a
    // valid memory region as this is where the LPC55 maps the DICE
    // registers. Support from the lpc55-pac would move the unsafe code
    // into the pac library:
    // https://github.com/lpc55/lpc55-pac/issues/15
    // https://github.com/lpc55/lpc55-pac/pull/16
    unsafe {
        slice::from_raw_parts_mut(
            REG_ADDR_NONSEC as *mut u32,
            SEED_LENGTH / mem::size_of::<u32>(),
        )
    }
}

/// NXP LPC55 UM 11126 §4.5.74 states: "Once CDI is computed and consumed,
/// contents of those registers will be erased by ROM." Testing has shown
/// however that the DICE registers are not cleared after they're read.
/// This type is for accessing and clearing the NXP DICE registers (aka the
/// CDI). To ensure the DICE registers are cleared, this type derives the
/// ZeroizeOnDrop trait. When an instance of this object goes out of scope
/// the register is cleared through the slice held.
#[derive(Zeroize, ZeroizeOnDrop)]
struct CdiReg(&'static mut [u32]);

impl Default for CdiReg {
    fn default() -> Self {
        Self(get_cdi_reg_slice())
    }
}

impl CdiReg {
    fn is_clear(&self) -> bool {
        self.0.iter().all(|&w| w == 0)
    }
}

pub trait SeedBuf {
    fn as_bytes(&self) -> &[u8; SEED_LENGTH];
}

/// This type is a thin wrapper around a byte array holding the CDI.
/// It's populated from the DICE registers by calling the 'from_reg'
/// constructor. The DICE registers are cleared immediately after the
/// CDI is read out from them and future attempts to construct a Cdi
/// instance through 'from_reg' will return None.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Cdi([u8; SEED_LENGTH]);

impl SeedBuf for Cdi {
    fn as_bytes(&self) -> &[u8; SEED_LENGTH] {
        &self.0
    }
}

impl Cdi {
    /// If DICE is disabled this function will return None. Otherwise it
    /// copies the contents of the DICE registers into a Cdi instance that's
    /// returned to the caller before clearing the DICE registers. This side
    /// effect causes subsequent calls to this function to return None.
    pub fn from_reg() -> Option<Self> {
        let mut cdi = [0u8; SEED_LENGTH];
        // If the CDI register hasn't already been cleared it will be when
        // this instance goes out of scope.
        let cdi_reg = CdiReg::default();

        // When registers holding CDI have been cleared / zeroed return None
        // to prevent unsuspecting consumers from deriving keys from 0's.
        if cdi_reg.is_clear() {
            return None;
        }

        for (dst, src) in cdi
            .chunks_exact_mut(mem::size_of::<u32>())
            .zip(cdi_reg.0.as_ref())
        {
            dst.copy_from_slice(&src.to_ne_bytes());
        }

        Some(Self(cdi))
    }
}

/// This function creates output keying material (OKM) using the Hkdf-extract-
/// and-expand to expand the seed using the provided info. The extract step is
/// skipped and so the seed MUST be sufficiently strong cryptographically for
/// use as a key itself (see RFC 5869 §3.3).
fn okm_from_seed_no_extract<S: SeedBuf>(
    seed: &S,
    info: &[u8],
) -> [u8; SEED_LENGTH] {
    let mut okm = [0u8; SEED_LENGTH];
    let hk =
        Hkdf::<Sha3_256>::from_prk(seed.as_bytes()).expect("Hkdf::from_prk");
    // TODO: error handling
    hk.expand(info, &mut okm).expect("failed to expand");

    okm
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct DeviceIdOkm([u8; SEED_LENGTH]);

impl DeviceIdOkm {
    // Use HKDF to to generate output keying material from CDI.
    // This assumes that the CDI is sufficiently random that no seed is
    // required (lpc55 uses PUF to create UDS).
    pub fn from_cdi(cdi: &Cdi) -> Self {
        Self(okm_from_seed_no_extract(cdi, "identity".as_bytes()))
    }

    pub fn as_bytes(&self) -> &[u8; SEED_LENGTH] {
        &self.0
    }
}

#[repr(C)]
#[derive(AsBytes, Default)]
pub struct CertSerialNumber(u8);

impl CertSerialNumber {
    pub fn new(csn: u8) -> Self {
        Self(csn)
    }

    pub fn next(&mut self) -> Self {
        let next = Self(self.0);
        self.0 += 1;

        next
    }
}

#[repr(C)]
#[derive(
    AsBytes, Clone, Copy, Debug, Deserialize, Serialize, SerializedSize,
)]
pub struct SerialNumber([u8; SN_LENGTH]);

#[derive(Clone, Copy, Debug)]
pub enum SNError {
    BadSize,
}

impl FromStr for SerialNumber {
    type Err = SNError;

    // TODO: Limit serialNumber values to PrintableStrings per x.520
    // https://github.com/oxidecomputer/hubris/issues/735
    fn from_str(sn: &str) -> Result<Self, Self::Err> {
        let sn: [u8; SN_LENGTH] =
            sn.as_bytes().try_into().map_err(|_| SNError::BadSize)?;
        Ok(Self(sn))
    }
}

impl SerialNumber {
    pub fn from_bytes(sn: &[u8; SN_LENGTH]) -> Self {
        Self(*sn)
    }
}

/// CdiL1 is a type that represents the compound device identifier (CDI) that's
/// derived from the Cdi and a seed value. This seed value must be the TCB
/// component identifier (TCI) representing the layer 1 (L1) firmware. This is
/// the hash of the Hubris image stage0 will attempt to boot.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct CdiL1([u8; SEED_LENGTH]);

impl SeedBuf for CdiL1 {
    fn as_bytes(&self) -> &[u8; SEED_LENGTH] {
        &self.0
    }
}

impl CdiL1 {
    pub fn new(cdi: &Cdi, tcb_tci: &[u8; SEED_LENGTH]) -> Self {
        let mut okm = [0u8; SEED_LENGTH];
        // TODO: Not sure FWID should be the salt here as we don't know much
        // about its entropy content (hash of FW). The CDI should have good
        // entropy though so maybe the CDI should be the salt and the FWID
        // should be the IKM? Does it matter?
        let hk = Hkdf::<Sha3_256>::new(Some(tcb_tci), cdi.as_bytes());
        // No info provided to 'expand', see RFC 5869 §3.2.
        // TODO: return error instead of expect
        hk.expand(&[], &mut okm).expect("failed to expand");

        CdiL1(okm)
    }
}

/// AliasOkm is a type that represents the output keying material (OKM) used
/// to create the Alias key. This key is used to attest to the measurements
/// collected by the platform.
#[derive(Deserialize, Serialize, SerializedSize, Zeroize, ZeroizeOnDrop)]
pub struct AliasOkm([u8; SEED_LENGTH]);

impl SeedBuf for AliasOkm {
    fn as_bytes(&self) -> &[u8; SEED_LENGTH] {
        &self.0
    }
}

impl AliasOkm {
    // keys derived from CDI_L1 here use HKDF w/ CDI_L1 as IKM, no salt
    // in extract, and info string in expand.
    pub fn from_cdi(cdi: &CdiL1) -> Self {
        Self(okm_from_seed_no_extract(cdi, "attestation".as_bytes()))
    }
}
