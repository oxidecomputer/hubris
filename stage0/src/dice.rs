// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::image_header::Image;
use crate::Handoff;
use core::convert::TryInto;
use dice_crate::{
    AliasCertBuilder, AliasData, AliasOkm, Cdi, CdiL1, CertSerialNumber,
    DeviceIdOkm, RngData, RngSeed, SeedBuf, SerialNumber, SpMeasureCertBuilder,
    SpMeasureData, SpMeasureOkm, TrustQuorumDheCertBuilder, TrustQuorumDheOkm,
};
use lpc55_pac::Peripherals;
use salty::signature::Keypair;
use sha3::{Digest, Sha3_256};
use unwrap_lite::UnwrapLite;

#[cfg(feature = "dice-self")]
use crate::dice_mfg_self::gen_mfg_artifacts;
#[cfg(feature = "dice-mfg")]
use crate::dice_mfg_usart::gen_mfg_artifacts;

fn gen_deviceid_keypair(cdi: &Cdi) -> Keypair {
    let devid_okm = DeviceIdOkm::from_cdi(cdi);

    Keypair::from(devid_okm.as_bytes())
}

pub struct SerialNumbers {
    pub cert_serial_number: CertSerialNumber,
    pub serial_number: SerialNumber,
}

fn gen_alias_artifacts(
    cdi_l1: &CdiL1,
    cert_serial_number: &mut CertSerialNumber,
    serial_number: &SerialNumber,
    deviceid_keypair: &Keypair,
    fwid: &[u8; 32],
    handoff: &Handoff,
) {
    let alias_okm = AliasOkm::from_cdi(&cdi_l1);
    let alias_keypair = Keypair::from(alias_okm.as_bytes());

    let alias_cert = AliasCertBuilder::new(
        &cert_serial_number.next(),
        &serial_number,
        &alias_keypair.public,
        fwid,
    )
    .sign(&deviceid_keypair);

    let tqdhe_okm = TrustQuorumDheOkm::from_cdi(&cdi_l1);
    let tqdhe_keypair = Keypair::from(tqdhe_okm.as_bytes());

    let tqdhe_cert = TrustQuorumDheCertBuilder::new(
        &cert_serial_number.next(),
        &serial_number,
        &tqdhe_keypair.public,
        fwid,
    )
    .sign(&deviceid_keypair);

    let alias_data =
        AliasData::new(alias_okm, alias_cert, tqdhe_okm, tqdhe_cert);

    handoff.store(&alias_data);
}

fn gen_spmeasure_artifacts(
    cdi_l1: &CdiL1,
    cert_serial_number: &mut CertSerialNumber,
    serial_number: &SerialNumber,
    deviceid_keypair: &Keypair,
    fwid: &[u8; 32],
    handoff: &Handoff,
) {
    let spmeasure_okm = SpMeasureOkm::from_cdi(&cdi_l1);
    let spmeasure_keypair = Keypair::from(spmeasure_okm.as_bytes());

    let spmeasure_cert = SpMeasureCertBuilder::new(
        &cert_serial_number.next(),
        &serial_number,
        &spmeasure_keypair.public,
        fwid,
    )
    .sign(&deviceid_keypair);

    let spmeasure_data = SpMeasureData::new(spmeasure_okm, spmeasure_cert);

    handoff.store(&spmeasure_data);
}

fn gen_rng_artifacts(cdi_l1: &CdiL1, handoff: &Handoff) {
    let rng_seed = RngSeed::from_cdi(cdi_l1);
    let rng_data = RngData::new(rng_seed);

    handoff.store(&rng_data);
}

fn gen_fwid(image: &Image) -> [u8; 32] {
    // Collect hash(es) of TCB. The first TCB Component Identifier (TCI)
    // calculated is the Hubris image. The DICE specs call this collection
    // of TCIs the FWID. This hash is stored in keeys certified by the
    // DeviceId. This hash should be 'updated' with relevant configuration
    // and code as FWID for Hubris becomes known.
    // TODO: This is a particularly naive way to calculate the FWID:
    // https://github.com/oxidecomputer/hubris/issues/736
    let mut fwid = Sha3_256::new();
    fwid.update(image.as_bytes());

    fwid.finalize().try_into().expect("fwid")
}

pub fn run(image: &Image, handoff: &Handoff) {
    // The memory we use to handoff DICE artifacts is already enabled
    // in `main()`;

    let cdi = match Cdi::from_reg() {
        Some(cdi) => cdi,
        None => return,
    };

    let deviceid_keypair = gen_deviceid_keypair(&cdi);

    let peripherals = Peripherals::take().unwrap_lite();
    let mut serial_numbers =
        gen_mfg_artifacts(&deviceid_keypair, &peripherals, handoff);

    let fwid = gen_fwid(image);

    // create CDI for layer 1 (L1) firmware (the hubris image we're booting)
    let cdi_l1 = CdiL1::new(&cdi, &fwid);

    gen_alias_artifacts(
        &cdi_l1,
        &mut serial_numbers.cert_serial_number,
        &serial_numbers.serial_number,
        &deviceid_keypair,
        &fwid,
        handoff,
    );

    gen_spmeasure_artifacts(
        &cdi_l1,
        &mut serial_numbers.cert_serial_number,
        &serial_numbers.serial_number,
        &deviceid_keypair,
        &fwid,
        handoff,
    );

    gen_rng_artifacts(&cdi_l1, handoff);
}
