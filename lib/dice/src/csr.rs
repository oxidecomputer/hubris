// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{persistid_csr_tmpl, SerialNumber};
use core::ops::Range;
use dice_mfg_msgs::SizedBlob;
use hubpack::SerializedSize;
use salty::constants::{
    PUBLICKEY_SERIALIZED_LENGTH, SIGNATURE_SERIALIZED_LENGTH,
};
use salty::signature::{Keypair, PublicKey};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use zerocopy::AsBytes;

// TODO: common trait to share with CertBuilder?
pub trait CsrBuilder {
    fn as_mut_bytes(&mut self) -> &mut [u8];

    fn set_range<T: AsBytes>(mut self, r: Range<usize>, t: &T) -> Self
    where
        Self: Sized,
    {
        self.as_mut_bytes()[r].copy_from_slice(t.as_bytes());

        self
    }

    const SUBJECT_SN_RANGE: Range<usize>;

    fn set_subject_sn(self, sn: &SerialNumber) -> Self
    where
        Self: Sized,
    {
        self.set_range(Self::SUBJECT_SN_RANGE, sn)
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

#[derive(Deserialize, Serialize, SerializedSize)]
pub struct PersistIdCsrBuilder(
    #[serde(with = "BigArray")] [u8; persistid_csr_tmpl::SIZE],
);

impl PersistIdCsrBuilder {
    pub fn new(dname_sn: &SerialNumber, public_key: &PublicKey) -> Self {
        Self(persistid_csr_tmpl::CSR_TMPL.clone())
            .set_subject_sn(dname_sn)
            .set_pub(public_key.as_bytes())
    }

    const SIGNDATA_RANGE: Range<usize> = persistid_csr_tmpl::SIGNDATA_RANGE;

    pub fn sign(self, keypair: &Keypair) -> SizedBlob
    where
        Self: Sized,
    {
        let signdata = &self.0[Self::SIGNDATA_RANGE];
        let sig = keypair.sign(signdata);
        let tmp = self.set_sig(&sig.to_bytes());

        SizedBlob::try_from(&tmp.0[..]).expect("csr sign")
    }
}

impl CsrBuilder for PersistIdCsrBuilder {
    const PUB_RANGE: Range<usize> = persistid_csr_tmpl::PUB_RANGE;
    const SUBJECT_SN_RANGE: Range<usize> = persistid_csr_tmpl::SUBJECT_SN_RANGE;
    const SIG_RANGE: Range<usize> = persistid_csr_tmpl::SIG_RANGE;

    fn as_mut_bytes(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

#[derive(Deserialize, Serialize, SerializedSize)]
pub struct PersistIdCsr(
    #[serde(with = "BigArray")] [u8; persistid_csr_tmpl::SIZE],
);
