// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    alias_cert_tmpl, deviceid_cert_tmpl, persistid_cert_tmpl,
    spmeasure_cert_tmpl, trust_quorum_dhe_cert_tmpl, CertSerialNumber,
    PersistIdCert,
};
use core::ops::Range;
use dice_mfg_msgs::{PlatformId, SizedBlob, PLATFORM_ID_MAX_LEN};
use hubpack::SerializedSize;
use salty::constants::{
    PUBLICKEY_SERIALIZED_LENGTH, SIGNATURE_SERIALIZED_LENGTH,
};
use salty::signature::{Keypair, PublicKey};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use static_assertions as sa;
use unwrap_lite::UnwrapLite;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

#[derive(Debug)]
pub enum CertError {
    BadSig,
    NoPubKey,
    NoSig,
    NoSignData,
    TooSmall,
    NotFound,
    NoCn,
}

pub trait Cert: IntoBytes + Immutable {
    fn get_range<'a, T>(&'a self, r: Range<usize>) -> T
    where
        T: TryFrom<&'a [u8]>,
    {
        self.as_bytes()[r].try_into().unwrap_lite()
    }

    const SERIAL_NUMBER_RANGE: Range<usize>;

    fn get_serial_number(&self) -> CertSerialNumber {
        let csn: [u8; 1] = self.get_range(Self::SERIAL_NUMBER_RANGE);
        CertSerialNumber::new(csn[0])
    }

    const PUB_RANGE: Range<usize>;

    fn get_pub(&self) -> &[u8] {
        self.get_range(Self::PUB_RANGE)
    }

    const SIG_RANGE: Range<usize>;

    fn get_sig(&self) -> &[u8] {
        self.get_range(Self::SIG_RANGE)
    }

    const SIGNDATA_RANGE: Range<usize>;

    fn get_signdata(&self) -> &[u8] {
        self.get_range(Self::SIGNDATA_RANGE)
    }
}

/// Trait for Certs with the TCG DICE TcbInfo structure w/ the FWID member.
pub trait FwidCert: Cert {
    const FWID_RANGE: Range<usize>;

    fn get_fwid(&self) -> &[u8] {
        self.get_range(Self::FWID_RANGE)
    }
}

pub trait CertBuilder: IntoBytes + FromBytes {
    fn set_range<T>(mut self, r: Range<usize>, t: &T) -> Self
    where
        T: IntoBytes + Immutable,
        Self: Sized,
    {
        self.as_mut_bytes()[r].copy_from_slice(t.as_bytes());

        self
    }

    const SERIAL_NUMBER_RANGE: Range<usize>;

    fn set_serial_number(self, sn: &CertSerialNumber) -> Self
    where
        Self: Sized,
    {
        self.set_range(Self::SERIAL_NUMBER_RANGE, sn)
    }

    const PUB_RANGE: Range<usize>;

    fn set_pub(self, pubkey: &[u8; PUBLICKEY_SERIALIZED_LENGTH]) -> Self
    where
        Self: Sized,
    {
        self.set_range(Self::PUB_RANGE, pubkey)
    }

    const SIG_RANGE: Range<usize>;

    fn set_sig(self, sig: &[u8; SIGNATURE_SERIALIZED_LENGTH]) -> Self
    where
        Self: Sized,
    {
        self.set_range(Self::SIG_RANGE, sig)
    }
}

pub trait IssuerCnCertBuilder: CertBuilder {
    const ISSUER_CN_RANGE: Range<usize>;

    fn set_issuer_cn(mut self, pid: &PlatformId) -> Self
    where
        Self: Sized,
    {
        let cn_range = Range {
            start: Self::ISSUER_CN_RANGE.start,
            // Account for possibility that PlatformId may be smaller than
            // the full ISSUER_CN_RANGE.
            end: Self::ISSUER_CN_RANGE.start + pid.as_bytes().len(),
        };

        self.as_mut_bytes()[cn_range].copy_from_slice(pid.as_bytes());

        self
    }
}

pub trait SubjectCnCertBuilder: CertBuilder {
    const SUBJECT_CN_RANGE: Range<usize>;

    fn set_subject_cn(mut self, pid: &PlatformId) -> Self
    where
        Self: Sized,
    {
        let cn_range = Range {
            start: Self::SUBJECT_CN_RANGE.start,
            // Account for possibility that PlatformId may be smaller than
            // the full SUBJECT_CN_RANGE.
            end: Self::SUBJECT_CN_RANGE.start + pid.as_bytes().len(),
        };

        self.as_mut_bytes()[cn_range].copy_from_slice(pid.as_bytes());

        self
    }
}

// Several functions in this module return arrays with the following lengths.
// These consts are a work around to keep from having to enable an unstable
// feature: generic_const_exprs.
const FWID_LENGTH: usize =
    alias_cert_tmpl::FWID_RANGE.end - alias_cert_tmpl::FWID_RANGE.start;

/// Trait for Certs with the TCG DICE TcbInfo structure w/ the FWID member.
pub trait FwidCertBuilder: CertBuilder {
    const FWID_RANGE: Range<usize>;

    fn set_fwid(self, fwid: &[u8; FWID_LENGTH]) -> Self
    where
        Self: Sized,
    {
        self.set_range(Self::FWID_RANGE, fwid)
    }
}

#[derive(IntoBytes, FromBytes)]
#[repr(C)]
pub struct PersistIdSelfCertBuilder([u8; persistid_cert_tmpl::SIZE]);

impl PersistIdSelfCertBuilder {
    pub fn new(
        cert_sn: &CertSerialNumber,
        pid: &PlatformId,
        public_key: &PublicKey,
    ) -> Self {
        Self(persistid_cert_tmpl::CERT_TMPL)
            .set_issuer_cn(pid)
            .set_subject_cn(pid)
            .set_serial_number(cert_sn)
            .set_pub(public_key.as_bytes())
    }

    pub fn sign(self, keypair: &Keypair) -> PersistIdCert
    where
        Self: Sized,
    {
        let signdata = &self.0[persistid_cert_tmpl::SIGNDATA_RANGE];
        let sig = keypair.sign(signdata);
        let tmp = self.set_sig(&sig.to_bytes());

        // We know the size of the cert we've generated but in the normal mfg
        // flow we won't so we wrap the generated cert in a more flexible type.
        PersistIdCert(SizedBlob::try_from(&tmp.0[..]).unwrap())
    }
}

impl CertBuilder for PersistIdSelfCertBuilder {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        persistid_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = persistid_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = persistid_cert_tmpl::SIG_RANGE;
}

sa::const_assert!(
    PLATFORM_ID_MAX_LEN
        == persistid_cert_tmpl::ISSUER_CN_RANGE.end
            - persistid_cert_tmpl::ISSUER_CN_RANGE.start
);

impl IssuerCnCertBuilder for PersistIdSelfCertBuilder {
    const ISSUER_CN_RANGE: Range<usize> = persistid_cert_tmpl::ISSUER_CN_RANGE;
}

sa::const_assert!(
    PLATFORM_ID_MAX_LEN
        == persistid_cert_tmpl::SUBJECT_CN_RANGE.end
            - persistid_cert_tmpl::SUBJECT_CN_RANGE.start
);

impl SubjectCnCertBuilder for PersistIdSelfCertBuilder {
    const SUBJECT_CN_RANGE: Range<usize> =
        persistid_cert_tmpl::SUBJECT_CN_RANGE;
}

// TODO: this type is brittle: The subject name in the persistent id cert
// MUST match the issuer
#[derive(IntoBytes, FromBytes)]
#[repr(C)]
pub struct DeviceIdCertBuilder([u8; deviceid_cert_tmpl::SIZE]);

impl DeviceIdCertBuilder {
    pub fn new(
        cert_sn: &CertSerialNumber,
        dname_cn: &PlatformId,
        public_key: &PublicKey,
    ) -> Self {
        Self(deviceid_cert_tmpl::CERT_TMPL)
            .set_serial_number(cert_sn)
            .set_issuer_cn(dname_cn)
            .set_pub(public_key.as_bytes())
    }

    pub fn sign(self, keypair: &Keypair) -> DeviceIdCert
    where
        Self: Sized,
    {
        let signdata = &self.0[deviceid_cert_tmpl::SIGNDATA_RANGE];
        let sig = keypair.sign(signdata);
        let tmp = self.set_sig(&sig.to_bytes());

        DeviceIdCert(tmp.0)
    }
}

impl CertBuilder for DeviceIdCertBuilder {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        deviceid_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = deviceid_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = deviceid_cert_tmpl::SIG_RANGE;
}

sa::const_assert!(
    PLATFORM_ID_MAX_LEN
        == deviceid_cert_tmpl::ISSUER_CN_RANGE.end
            - deviceid_cert_tmpl::ISSUER_CN_RANGE.start
);

impl IssuerCnCertBuilder for DeviceIdCertBuilder {
    const ISSUER_CN_RANGE: Range<usize> = deviceid_cert_tmpl::ISSUER_CN_RANGE;
}

#[derive(
    IntoBytes,
    Deserialize,
    FromBytes,
    Immutable,
    KnownLayout,
    Serialize,
    SerializedSize,
)]
#[repr(C)]
pub struct DeviceIdCert(
    #[serde(with = "BigArray")] [u8; deviceid_cert_tmpl::SIZE],
);

impl Cert for DeviceIdCert {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        deviceid_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = deviceid_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = deviceid_cert_tmpl::SIG_RANGE;
    const SIGNDATA_RANGE: Range<usize> = deviceid_cert_tmpl::SIGNDATA_RANGE;
}

#[derive(IntoBytes, FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct AliasCertBuilder([u8; alias_cert_tmpl::SIZE]);

impl AliasCertBuilder {
    pub fn new(
        cert_sn: &CertSerialNumber,
        public_key: &PublicKey,
        fwid: &[u8; FWID_LENGTH],
    ) -> Self {
        Self(alias_cert_tmpl::CERT_TMPL)
            .set_serial_number(cert_sn)
            .set_pub(public_key.as_bytes())
            .set_fwid(fwid)
    }

    pub fn sign(self, keypair: &Keypair) -> AliasCert
    where
        Self: Sized,
    {
        let signdata = &self.0[alias_cert_tmpl::SIGNDATA_RANGE];
        let sig = keypair.sign(signdata);
        let tmp = self.set_sig(&sig.to_bytes());

        AliasCert(tmp.0)
    }
}

impl CertBuilder for AliasCertBuilder {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        alias_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = alias_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = alias_cert_tmpl::SIG_RANGE;
}

impl FwidCertBuilder for AliasCertBuilder {
    const FWID_RANGE: Range<usize> = alias_cert_tmpl::FWID_RANGE;
}

#[derive(
    IntoBytes,
    Immutable,
    KnownLayout,
    Deserialize,
    FromBytes,
    Serialize,
    SerializedSize,
)]
#[repr(C)]
pub struct AliasCert(#[serde(with = "BigArray")] [u8; alias_cert_tmpl::SIZE]);

impl Cert for AliasCert {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        alias_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = alias_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = alias_cert_tmpl::SIG_RANGE;
    const SIGNDATA_RANGE: Range<usize> = alias_cert_tmpl::SIGNDATA_RANGE;
}

impl FwidCert for AliasCert {
    const FWID_RANGE: Range<usize> = alias_cert_tmpl::FWID_RANGE;
}

#[derive(IntoBytes, FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct SpMeasureCertBuilder([u8; spmeasure_cert_tmpl::SIZE]);

impl SpMeasureCertBuilder {
    pub fn new(
        cert_sn: &CertSerialNumber,
        public_key: &PublicKey,
        fwid: &[u8; FWID_LENGTH],
    ) -> Self {
        Self(spmeasure_cert_tmpl::CERT_TMPL)
            .set_serial_number(cert_sn)
            .set_pub(public_key.as_bytes())
            .set_fwid(fwid)
    }

    pub fn sign(self, keypair: &Keypair) -> SpMeasureCert
    where
        Self: Sized,
    {
        let signdata = &self.0[spmeasure_cert_tmpl::SIGNDATA_RANGE];
        let sig = keypair.sign(signdata);
        let tmp = self.set_sig(&sig.to_bytes());

        SpMeasureCert(tmp.0)
    }
}

impl CertBuilder for SpMeasureCertBuilder {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        spmeasure_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = spmeasure_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = spmeasure_cert_tmpl::SIG_RANGE;
}

impl FwidCertBuilder for SpMeasureCertBuilder {
    const FWID_RANGE: Range<usize> = spmeasure_cert_tmpl::FWID_RANGE;
}

#[derive(
    IntoBytes,
    Immutable,
    KnownLayout,
    Deserialize,
    FromBytes,
    Serialize,
    SerializedSize,
)]
#[repr(C)]
pub struct SpMeasureCert(
    #[serde(with = "BigArray")] [u8; spmeasure_cert_tmpl::SIZE],
);

impl Cert for SpMeasureCert {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        spmeasure_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = spmeasure_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = spmeasure_cert_tmpl::SIG_RANGE;
    const SIGNDATA_RANGE: Range<usize> = spmeasure_cert_tmpl::SIGNDATA_RANGE;
}

sa::const_assert!(
    FWID_LENGTH
        == spmeasure_cert_tmpl::FWID_RANGE.end
            - spmeasure_cert_tmpl::FWID_RANGE.start
);

impl FwidCert for SpMeasureCert {
    const FWID_RANGE: Range<usize> = spmeasure_cert_tmpl::FWID_RANGE;
}

#[derive(IntoBytes, FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct TrustQuorumDheCertBuilder([u8; trust_quorum_dhe_cert_tmpl::SIZE]);

impl TrustQuorumDheCertBuilder {
    pub fn new(
        cert_sn: &CertSerialNumber,
        public_key: &PublicKey,
        fwid: &[u8; FWID_LENGTH],
    ) -> Self {
        Self(trust_quorum_dhe_cert_tmpl::CERT_TMPL)
            .set_serial_number(cert_sn)
            .set_pub(public_key.as_bytes())
            .set_fwid(fwid)
    }

    pub fn sign(self, keypair: &Keypair) -> TrustQuorumDheCert
    where
        Self: Sized,
    {
        let signdata = &self.0[trust_quorum_dhe_cert_tmpl::SIGNDATA_RANGE];
        let sig = keypair.sign(signdata);
        let tmp = self.set_sig(&sig.to_bytes());

        TrustQuorumDheCert(tmp.0)
    }
}

impl CertBuilder for TrustQuorumDheCertBuilder {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        trust_quorum_dhe_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = trust_quorum_dhe_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = trust_quorum_dhe_cert_tmpl::SIG_RANGE;
}

impl FwidCertBuilder for TrustQuorumDheCertBuilder {
    const FWID_RANGE: Range<usize> = trust_quorum_dhe_cert_tmpl::FWID_RANGE;
}

#[derive(
    IntoBytes,
    Deserialize,
    FromBytes,
    Immutable,
    KnownLayout,
    Serialize,
    SerializedSize,
)]
#[repr(C)]
pub struct TrustQuorumDheCert(
    #[serde(with = "BigArray")] [u8; trust_quorum_dhe_cert_tmpl::SIZE],
);

impl Cert for TrustQuorumDheCert {
    const SERIAL_NUMBER_RANGE: Range<usize> =
        trust_quorum_dhe_cert_tmpl::SERIAL_NUMBER_RANGE;
    const PUB_RANGE: Range<usize> = trust_quorum_dhe_cert_tmpl::PUB_RANGE;
    const SIG_RANGE: Range<usize> = trust_quorum_dhe_cert_tmpl::SIG_RANGE;
    const SIGNDATA_RANGE: Range<usize> =
        trust_quorum_dhe_cert_tmpl::SIGNDATA_RANGE;
}

sa::const_assert!(
    FWID_LENGTH
        == trust_quorum_dhe_cert_tmpl::FWID_RANGE.end
            - trust_quorum_dhe_cert_tmpl::FWID_RANGE.start
);

impl FwidCert for TrustQuorumDheCert {
    const FWID_RANGE: Range<usize> = trust_quorum_dhe_cert_tmpl::FWID_RANGE;
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;

    #[test]
    fn serial_number_from_new() {
        let sn = CertSerialNumber::new(0x10);
        let cert = PersistIdSelfCert([0u8; persistid_cert_tmpl::SIZE])
            .set_serial_number(&sn);

        assert_eq!(sn, cert.get_serial_number());
    }

    #[test]
    fn issuer_sn_from_new() {
        let sn = SerialNumber::from_str("0123456789ab").expect("SN from_str");
        let cert = PersistIdSelfCert([0u8; persistid_cert_tmpl::SIZE])
            .set_issuer_sn(&sn);

        assert_eq!(cert.get_issuer_sn().as_bytes(), sn.as_bytes());
    }

    #[test]
    fn subject_sn_from_new() {
        let sn = SerialNumber::from_str("0123456789ab").expect("SN from_str");
        let cert = PersistIdSelfCert([0u8; persistid_cert_tmpl::SIZE])
            .set_subject_sn(&sn);

        assert_eq!(cert.get_subject_sn().as_bytes(), sn.as_bytes());
    }

    // Signature over CERT with issuer / subject SN & PUBKEY set according
    // to 'sign' test below.
    const SIG_EXPECTED: [u8; SIGNATURE_SERIALIZED_LENGTH] = [
        0x26, 0x2A, 0x81, 0xE9, 0x1F, 0x06, 0xCF, 0xF0, 0x13, 0xEB, 0x33, 0x71,
        0x5A, 0xB9, 0x5C, 0xC0, 0xC7, 0x40, 0x01, 0x83, 0x7C, 0xB6, 0x2F, 0x2E,
        0x88, 0xE9, 0x95, 0xD9, 0x10, 0x9C, 0xD8, 0xF5, 0x33, 0x4B, 0x9B, 0xB1,
        0x6A, 0xB3, 0x23, 0xDB, 0x3A, 0x1C, 0x35, 0x31, 0xE7, 0x38, 0xEC, 0x9B,
        0xAA, 0x32, 0x36, 0x5A, 0xAA, 0x37, 0x4B, 0xF5, 0xE7, 0x7A, 0x2C, 0x4E,
        0x88, 0x35, 0x50, 0x0E,
    ];

    #[test]
    fn sign() {
        // well known seed
        let seed: [u8; 32] = [42; 32];
        let keypair: salty::Keypair = salty::Keypair::from(&seed);

        let sn = SerialNumber::from_str("0123456789ab").expect("SN from_str");
        let cert_sn = CertSerialNumber::new(0);
        let cert = PersistIdSelfCert::new(&cert_sn, &sn, &keypair);

        for (index, byte) in cert.as_bytes()[persistid_cert_tmpl::SIG_RANGE]
            .iter()
            .enumerate()
        {
            if index % 12 == 11 {
                println!("{:#04X},", byte);
            } else {
                print!("{:#04X}, ", byte);
            }
        }
        assert_eq!(cert.0[persistid_cert_tmpl::SIG_RANGE], SIG_EXPECTED);
    }
}
