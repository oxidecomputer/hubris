// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![cfg_attr(not(test), no_std)]

use core::mem;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use hubpack::SerializedSize;
use salty::constants::SECRETKEY_SEED_LENGTH;
use serde::{Deserialize, Serialize};
use sha3::Sha3_256;
use vcell::VolatileCell;
use zerocopy::{Immutable, IntoBytes, KnownLayout};
use zeroize::{Zeroize, ZeroizeOnDrop};

// re-export useful types from dice-mfg-msgs making them part of our API
pub use dice_mfg_msgs::{PlatformId, SizedBlob};

mod cert;
pub use crate::cert::{
    AliasCert, AliasCertBuilder, Cert, CertError, DeviceIdCert,
    DeviceIdCertBuilder, FwidCert, PersistIdSelfCertBuilder, SpMeasureCert,
    SpMeasureCertBuilder, TrustQuorumDheCert, TrustQuorumDheCertBuilder,
};
mod csr;
pub use crate::csr::PersistIdCsrBuilder;
mod alias_cert_tmpl;
mod deviceid_cert_tmpl;
mod handoff;
mod mfg;
mod persistid_cert_tmpl;
mod persistid_csr_tmpl;
pub use crate::mfg::{
    DiceMfg, DiceMfgState, PersistIdSeed, SelfMfg, SerialMfg,
};
mod spmeasure_cert_tmpl;
mod trust_quorum_dhe_cert_tmpl;
pub use crate::handoff::{AliasData, CertData, RngData, SpMeasureData};

pub const SEED_LENGTH: usize = SECRETKEY_SEED_LENGTH;

/// Retrieves the bank of CDI registers from the SYSCON as a slice of volatile
/// cells.
fn get_cdi_reg_slice(
    syscon: &lpc55_pac::syscon::RegisterBlock,
) -> &[VolatileCell<u32>; 8] {
    // The PAC doesn't correctly model the CDI registers in the SYSCON, so we
    // need to resort to pointer arithmetic. The registers start at offset 0x900
    // (in bytes) past the SYSCON.
    let base = syscon as *const _ as *const u32;
    // Safety: this is unsafe because the pointer calculation can wrap, but in
    // our case, we know the SYSCON is much bigger than 0x900 bytes.
    let cdi_addr = unsafe { base.add(0x900 / mem::size_of::<u32>()) };

    // Safety: we're punning the raw pointer to an array reference here, which
    // is ok only because we know this part of the syscon contains an 8-register
    // array. Since the registers are modeled using VolatileCell, aliasing is
    // acceptable, though the returned value of this function still borrows
    // `syscon` just to keep the caller honest.
    unsafe { &*(cdi_addr as *const [_; 8]) }
}

/// NXP LPC55 UM 11126 §4.5.74 states: "Once CDI is computed and consumed,
/// contents of those registers will be erased by ROM." Testing has shown
/// however that the DICE registers are not cleared after they're read.
/// This type is for accessing and clearing the NXP DICE registers (aka the
/// CDI). To ensure the DICE registers are cleared, this type derives the
/// ZeroizeOnDrop trait. When an instance of this object goes out of scope
/// the register is cleared through the slice held.
struct CdiReg<'a>(&'a [VolatileCell<u32>; 8]);

// The zeroize crate knows nothing of the vcell crate, so we have to implement
// all of this by hand. Note that it's _really easy_ to break this, be careful.
impl Zeroize for CdiReg<'_> {
    fn zeroize(&mut self) {
        for register in self.0 {
            register.set(0);
        }
    }
}

impl Drop for CdiReg<'_> {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl ZeroizeOnDrop for CdiReg<'_> {}

impl<'a> CdiReg<'a> {
    fn from_syscon(syscon: &'a lpc55_pac::syscon::RegisterBlock) -> Self {
        Self(get_cdi_reg_slice(syscon))
    }
    fn is_clear(&self) -> bool {
        self.0.iter().all(|w| w.get() == 0)
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
    pub fn from_reg(syscon: &lpc55_pac::syscon::RegisterBlock) -> Option<Self> {
        let mut cdi = [0u8; SEED_LENGTH];
        // If the CDI register hasn't already been cleared it will be when
        // this instance goes out of scope.
        let cdi_reg = CdiReg::from_syscon(syscon);

        // When registers holding CDI have been cleared / zeroed return None
        // to prevent unsuspecting consumers from deriving keys from 0's.
        if cdi_reg.is_clear() {
            return None;
        }

        for (dst, src) in cdi
            .chunks_exact_mut(mem::size_of::<u32>())
            .zip(cdi_reg.0.as_ref())
        {
            dst.copy_from_slice(&src.get().to_ne_bytes());
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

// TODO: Start CertSerialNumber from > 0. RFD 5280 4.1.2.2: must be positive
// integer (does not include 0).
#[repr(C)]
#[derive(IntoBytes, Immutable, KnownLayout, Default)]
pub struct CertSerialNumber(u8);

impl CertSerialNumber {
    pub fn new(csn: u8) -> Self {
        Self(csn)
    }

    pub fn next_num(&mut self) -> Self {
        let next = Self(self.0);
        self.0 += 1;

        next
    }
}

/// CdiL1 is a type that represents the compound device identifier (CDI) for
/// the layer 1 (L1) software. The CdiL1 value is constructed from Cdi and the
/// TCB component identifier (TCI) representing the layer 1 software.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct CdiL1([u8; SEED_LENGTH]);

impl SeedBuf for CdiL1 {
    fn as_bytes(&self) -> &[u8; SEED_LENGTH] {
        &self.0
    }
}

impl CdiL1 {
    pub fn new(cdi: &Cdi, tci: &[u8; SEED_LENGTH]) -> Self {
        let mut hmac =
            Hmac::<Sha3_256>::new_from_slice(cdi.as_bytes()).unwrap();
        hmac.update(tci);

        let result = hmac.finalize();
        CdiL1(result.into_bytes().into())
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

/// SpMeasureOkm is a type that represents the output keying material (OKM) used
/// to create the SpMeasure key. This key is used as an embedded CA by the task
/// that measures the service processor sofware image.
#[derive(Deserialize, Serialize, SerializedSize, Zeroize, ZeroizeOnDrop)]
pub struct SpMeasureOkm([u8; SEED_LENGTH]);

impl SeedBuf for SpMeasureOkm {
    fn as_bytes(&self) -> &[u8; SEED_LENGTH] {
        &self.0
    }
}

impl SpMeasureOkm {
    // keys derived from CDI_L1 here use HKDF w/ CDI_L1 as IKM, no salt
    // in extract, and info string in expand.
    pub fn from_cdi(cdi: &CdiL1) -> Self {
        Self(okm_from_seed_no_extract(cdi, "sp-measure".as_bytes()))
    }
}

/// TrustQuorumDheOkm is a type that represents the output keying material
/// (OKM)used to create the trust quorum DHE key. This key is used as the
/// identity key in the trust quorum DHE.
#[derive(Deserialize, Serialize, SerializedSize, Zeroize, ZeroizeOnDrop)]
pub struct TrustQuorumDheOkm([u8; SEED_LENGTH]);

impl SeedBuf for TrustQuorumDheOkm {
    fn as_bytes(&self) -> &[u8; SEED_LENGTH] {
        &self.0
    }
}

impl TrustQuorumDheOkm {
    // keys derived from CDI_L1 here use HKDF w/ CDI_L1 as IKM, no salt
    // in extract, and info string in expand.
    pub fn from_cdi(cdi: &CdiL1) -> Self {
        Self(okm_from_seed_no_extract(cdi, "trust-quorum-dhe".as_bytes()))
    }
}

#[derive(Deserialize, Serialize, SerializedSize, Zeroize, ZeroizeOnDrop)]
pub struct RngSeed([u8; SEED_LENGTH]);

impl SeedBuf for RngSeed {
    fn as_bytes(&self) -> &[u8; SEED_LENGTH] {
        &self.0
    }
}

impl RngSeed {
    pub fn from_cdi(cdi: &CdiL1) -> Self {
        Self(okm_from_seed_no_extract(cdi, "entropy".as_bytes()))
    }
}

#[derive(Deserialize, Serialize, SerializedSize)]
pub struct PersistIdCert(pub SizedBlob);

#[derive(Deserialize, Serialize, SerializedSize)]
pub struct IntermediateCert(pub SizedBlob);
