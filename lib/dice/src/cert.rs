// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    alias_cert_tmpl, deviceid_cert_tmpl, SerialNumber, NOTBEFORE_LENGTH,
};
use hubpack::SerializedSize;
use salty::constants::{
    PUBLICKEY_SERIALIZED_LENGTH, SIGNATURE_SERIALIZED_LENGTH,
};
use salty::signature::{Keypair, PublicKey};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use unwrap_lite::UnwrapLite;

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

pub trait Cert {
    fn as_bytes(&self) -> &[u8];
    fn as_mut_bytes(&mut self) -> &mut [u8];

    const SERIAL_NUMBER_START: usize;
    const SERIAL_NUMBER_END: usize;

    fn get_serial_number(&self) -> u8 {
        u8::from_be_bytes(
            self.as_bytes()[Self::SERIAL_NUMBER_START..Self::SERIAL_NUMBER_END]
                .try_into()
                .unwrap_lite(),
        )
    }

    fn set_serial_number(mut self, sn: u8) -> Self
    where
        Self: Sized,
    {
        self.as_mut_bytes()[Self::SERIAL_NUMBER_START..Self::SERIAL_NUMBER_END]
            .copy_from_slice(&sn.to_be_bytes());

        self
    }

    const ISSUER_SN_START: usize;
    const ISSUER_SN_END: usize;

    fn get_issuer_sn(&self) -> SerialNumber {
        SerialNumber::from_bytes(
            &self.as_bytes()[Self::ISSUER_SN_START..Self::ISSUER_SN_END]
                .try_into()
                .unwrap_lite(),
        )
    }

    fn set_issuer_sn(mut self, sn: &SerialNumber) -> Self
    where
        Self: Sized,
    {
        self.as_mut_bytes()[Self::ISSUER_SN_START..Self::ISSUER_SN_END]
            .copy_from_slice(sn.as_bytes());

        self
    }

    const NOTBEFORE_START: usize;
    const NOTBEFORE_END: usize;

    fn set_notbefore(mut self, utctime: &[u8; NOTBEFORE_LENGTH]) -> Self
    where
        Self: Sized,
    {
        self.as_mut_bytes()[Self::NOTBEFORE_START..Self::NOTBEFORE_END]
            .copy_from_slice(utctime);

        self
    }

    const SUBJECT_SN_START: usize;
    const SUBJECT_SN_END: usize;

    fn get_subject_sn(&self) -> SerialNumber {
        SerialNumber::from_bytes(
            &self.as_bytes()[Self::SUBJECT_SN_START..Self::SUBJECT_SN_END]
                .try_into()
                .unwrap_lite(),
        )
    }

    fn set_subject_sn(mut self, sn: &SerialNumber) -> Self
    where
        Self: Sized,
    {
        self.as_mut_bytes()[Self::SUBJECT_SN_START..Self::SUBJECT_SN_END]
            .copy_from_slice(sn.as_bytes());

        self
    }

    const PUB_START: usize;
    const PUB_END: usize;

    fn get_pub(&self) -> &[u8] {
        &self.as_bytes()[Self::PUB_START..Self::PUB_END]
    }

    fn set_pub(mut self, pubkey: &[u8; PUBLICKEY_SERIALIZED_LENGTH]) -> Self
    where
        Self: Sized,
    {
        self.as_mut_bytes()[Self::PUB_START..Self::PUB_END]
            .copy_from_slice(pubkey);

        self
    }

    const SIG_START: usize;
    const SIG_END: usize;

    fn get_sig(&self) -> &[u8] {
        &self.as_bytes()[Self::SIG_START..Self::SIG_END]
    }

    fn set_sig(mut self, sig: &[u8; SIGNATURE_SERIALIZED_LENGTH]) -> Self
    where
        Self: Sized,
    {
        self.as_mut_bytes()[Self::SIG_START..Self::SIG_END]
            .copy_from_slice(sig);

        self
    }

    const SIGNDATA_START: usize;
    const SIGNDATA_END: usize;

    fn get_signdata(&self) -> &[u8] {
        &self.as_bytes()[Self::SIGNDATA_START..Self::SIGNDATA_END]
    }

    fn sign(self, keypair: &Keypair) -> Self
    where
        Self: Sized,
    {
        let signdata = self.get_signdata();
        let sig = keypair.sign(signdata);

        self.set_sig(&sig.to_bytes())
    }
}

#[derive(Deserialize, Serialize, SerializedSize)]
pub struct DeviceIdSelfCert(
    #[serde(with = "BigArray")] [u8; deviceid_cert_tmpl::SIZE],
);

impl DeviceIdSelfCert {
    pub fn new(
        cert_sn: u8,
        dname_sn: &SerialNumber,
        keypair: &Keypair,
    ) -> Self {
        Self(deviceid_cert_tmpl::CERT_TMPL.clone())
            .set_serial_number(cert_sn)
            .set_issuer_sn(dname_sn)
            .set_subject_sn(dname_sn)
            .set_pub(keypair.public.as_bytes())
            .sign(keypair)
    }
}

impl Cert for DeviceIdSelfCert {
    const SERIAL_NUMBER_START: usize = deviceid_cert_tmpl::SERIAL_NUMBER_START;
    const SERIAL_NUMBER_END: usize = deviceid_cert_tmpl::SERIAL_NUMBER_END;
    const ISSUER_SN_START: usize = deviceid_cert_tmpl::ISSUER_SN_START;
    const ISSUER_SN_END: usize = deviceid_cert_tmpl::ISSUER_SN_END;
    const NOTBEFORE_START: usize = deviceid_cert_tmpl::NOTBEFORE_START;
    const NOTBEFORE_END: usize = deviceid_cert_tmpl::NOTBEFORE_END;
    const SUBJECT_SN_START: usize = deviceid_cert_tmpl::SUBJECT_SN_START;
    const SUBJECT_SN_END: usize = deviceid_cert_tmpl::SUBJECT_SN_END;
    const PUB_START: usize = deviceid_cert_tmpl::PUB_START;
    const PUB_END: usize = deviceid_cert_tmpl::PUB_END;
    const SIG_START: usize = deviceid_cert_tmpl::SIG_START;
    const SIG_END: usize = deviceid_cert_tmpl::SIG_END;
    const SIGNDATA_START: usize = deviceid_cert_tmpl::SIGNDATA_START;
    const SIGNDATA_END: usize = deviceid_cert_tmpl::SIGNDATA_END;

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

#[derive(Deserialize, Serialize, SerializedSize)]
pub struct AliasCert(#[serde(with = "BigArray")] [u8; alias_cert_tmpl::SIZE]);

impl AliasCert {
    pub fn new(
        cert_sn: u8,
        dname_sn: &SerialNumber,
        public_key: &PublicKey,
        fwid: &[u8; alias_cert_tmpl::FWID_LENGTH],
        keypair: &Keypair,
    ) -> Self {
        Self(alias_cert_tmpl::CERT_TMPL.clone())
            .set_serial_number(cert_sn)
            .set_issuer_sn(dname_sn)
            .set_subject_sn(dname_sn)
            .set_pub(public_key.as_bytes())
            .set_fwid(fwid)
            .sign(keypair)
    }
}

impl Cert for AliasCert {
    const SERIAL_NUMBER_START: usize = alias_cert_tmpl::SERIAL_NUMBER_START;
    const SERIAL_NUMBER_END: usize = alias_cert_tmpl::SERIAL_NUMBER_END;
    const ISSUER_SN_START: usize = alias_cert_tmpl::ISSUER_SN_START;
    const ISSUER_SN_END: usize = alias_cert_tmpl::ISSUER_SN_END;
    const NOTBEFORE_START: usize = alias_cert_tmpl::NOTBEFORE_START;
    const NOTBEFORE_END: usize = alias_cert_tmpl::NOTBEFORE_END;
    const SUBJECT_SN_START: usize = alias_cert_tmpl::SUBJECT_SN_START;
    const SUBJECT_SN_END: usize = alias_cert_tmpl::SUBJECT_SN_END;
    const PUB_START: usize = alias_cert_tmpl::PUB_START;
    const PUB_END: usize = alias_cert_tmpl::PUB_END;
    const SIG_START: usize = alias_cert_tmpl::SIG_START;
    const SIG_END: usize = alias_cert_tmpl::SIG_END;
    const SIGNDATA_START: usize = alias_cert_tmpl::SIGNDATA_START;
    const SIGNDATA_END: usize = alias_cert_tmpl::SIGNDATA_END;

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl AliasCert {
    pub fn set_fwid(
        mut self,
        fwid: &[u8; alias_cert_tmpl::FWID_LENGTH],
    ) -> Self {
        self.0[alias_cert_tmpl::FWID_START..alias_cert_tmpl::FWID_END]
            .copy_from_slice(fwid);

        self
    }

    pub fn get_fwid(&self) -> &[u8; alias_cert_tmpl::FWID_LENGTH] {
        self.0[alias_cert_tmpl::FWID_START..alias_cert_tmpl::FWID_END]
            .try_into()
            .unwrap_lite()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;

    #[test]
    fn serial_number_from_new() {
        let sn: u8 = 0x10;
        let cert = DeviceIdSelfCert([0u8; deviceid_cert_tmpl::SIZE])
            .set_serial_number(sn);

        assert_eq!(sn, cert.get_serial_number());
    }

    #[test]
    fn issuer_sn_from_new() {
        let sn = SerialNumber::from_str("0123456789ab").expect("SN from_str");
        let cert = DeviceIdSelfCert([0u8; deviceid_cert_tmpl::SIZE])
            .set_issuer_sn(&sn);

        assert_eq!(cert.get_issuer_sn().as_bytes(), sn.as_bytes());
    }

    #[test]
    fn subject_sn_from_new() {
        let sn = SerialNumber::from_str("0123456789ab").expect("SN from_str");
        let cert = DeviceIdSelfCert([0u8; deviceid_cert_tmpl::SIZE])
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
        let cert = DeviceIdSelfCert::new(0, &sn, &keypair);

        for (index, byte) in cert.as_bytes()
            [deviceid_cert_tmpl::SIG_START..deviceid_cert_tmpl::SIG_END]
            .iter()
            .enumerate()
        {
            if index % 12 == 11 {
                println!("{:#04X},", byte);
            } else {
                print!("{:#04X}, ", byte);
            }
        }
        assert_eq!(
            cert.0[deviceid_cert_tmpl::SIG_START..deviceid_cert_tmpl::SIG_END],
            SIG_EXPECTED
        );
    }
}
