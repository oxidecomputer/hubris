// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Handoff;
use core::mem;
use drv_lpc55_flash::Flash;
use lib_dice::{
    AliasCertBuilder, AliasData, AliasOkm, Cdi, CdiL1, CertData,
    CertSerialNumber, DeviceIdCertBuilder, DeviceIdOkm, IntermediateCert,
    PersistIdCert, PlatformId, RngData, RngSeed, SeedBuf, SpMeasureCertBuilder,
    SpMeasureData, SpMeasureOkm, TrustQuorumDheCertBuilder, TrustQuorumDheOkm,
};
use lpc55_pac::Peripherals;
use lpc55_puf::Puf;
use salty::{constants::SECRETKEY_SEED_LENGTH, signature::Keypair};

#[cfg(feature = "dice-mfg")]
use crate::dice_mfg_usart;

pub const SEED_LEN: usize = SECRETKEY_SEED_LENGTH;
pub const KEYCODE_LEN: usize =
    Puf::key_to_keycode_len(SEED_LEN) / mem::size_of::<u32>();
pub const KEY_INDEX: u32 = 1;

/// Data we get back from the manufacturing process.
pub struct MfgResult {
    pub cert_serial_number: CertSerialNumber,
    pub platform_id: PlatformId,
    pub persistid_keypair: Keypair,
    pub persistid_cert: PersistIdCert,
    pub intermediate_cert: Option<IntermediateCert>,
}

/// Generate stuff associated with the manufacturing process.
fn gen_mfg_artifacts(
    peripherals: &Peripherals,
    flash: &mut Flash,
) -> MfgResult {
    // Select manufacturing process based on feature. This module assumes
    // that one of the manufacturing flavors has been enabled.
    cfg_if::cfg_if! {
        if #[cfg(feature = "dice-mfg")] {
            dice_mfg_usart::gen_mfg_artifacts_usart(peripherals, flash)
        } else if #[cfg(feature = "dice-self")] {
            gen_mfg_artifacts_self(peripherals, flash)
        } else {
            compile_error!("No DICE manufacturing process selected.");
        }
    }
}

/// This function defines the manufacturing process for self signed identity
/// certificates. This is expected to be useful for development systems that
/// cannot easily have identities certified by an external CA.
#[cfg(feature = "dice-self")]
fn gen_mfg_artifacts_self(
    peripherals: &Peripherals,
    _flash: &mut Flash,
) -> MfgResult {
    use core::ops::{Deref, DerefMut};
    use lib_dice::{DiceMfg, PersistIdSeed, SelfMfg};
    use zeroize::Zeroizing;

    let puf = Puf::new(&peripherals.PUF);

    // Create key code for an ed25519 seed using the PUF. We use this seed
    // to generate a key used as an identity that is independent from the
    // DICE measured boot.
    let mut keycode = Zeroizing::new([0u32; KEYCODE_LEN]);
    if !puf.generate_keycode(KEY_INDEX, SEED_LEN, keycode.deref_mut()) {
        panic!("failed to generate keycode");
    }
    let keycode = keycode;

    // get keycode from DICE MFG flash region
    let mut seed = [0u8; SEED_LEN];
    if !puf.get_key(keycode.deref(), &mut seed) {
        // failure to get this key isn't recoverable
        panic!("failed to get seed");
    }
    let seed = seed;

    // we're done with the puf: block the key index
    if !puf.block_index(KEY_INDEX) {
        panic!("failed to block PUF index");
    }
    // lock the lower key indices to prevent use of this index till reset
    puf.lock_indices_low();

    let id_seed = PersistIdSeed::new(seed);
    let id_keypair = Keypair::from(id_seed.as_bytes());

    let mfg_state = SelfMfg::new(&id_keypair).run();

    // Return new CertSerialNumber and platform serial number to caller.
    // These are used to fill in the templates for certs signed by the
    // DeviceId.
    MfgResult {
        cert_serial_number: Default::default(),
        platform_id: mfg_state.platform_id,
        persistid_keypair: id_keypair,
        persistid_cert: mfg_state.persistid_cert,
        intermediate_cert: mfg_state.intermediate_cert,
    }
}

/// Generate the DICE DeviceId key and certificate. The cert is never returned
/// to the caller, instead it's packaged up with the certs passed as
/// parameters and haded off to Hubris through the Handoff type.
fn gen_deviceid_artifacts(
    cdi: &Cdi,
    cert_serial_number: &mut CertSerialNumber,
    platform_id: &PlatformId,
    persistid_keypair: Keypair,
    persistid_cert: PersistIdCert,
    intermediate_cert: Option<IntermediateCert>,
    handoff: &Handoff,
) -> Keypair {
    let devid_okm = DeviceIdOkm::from_cdi(cdi);

    let deviceid_keypair = Keypair::from(devid_okm.as_bytes());

    let deviceid_cert = DeviceIdCertBuilder::new(
        &cert_serial_number.next_num(),
        &platform_id,
        &deviceid_keypair.public,
    )
    .sign(&persistid_keypair);

    // transfer certs to CertData for serialization
    let cert_data =
        CertData::new(deviceid_cert, persistid_cert, intermediate_cert);

    handoff.store(&cert_data);

    deviceid_keypair
}

/// Generate DICE keys and credentials that we pass to the root of trust
/// for reporting (RoT-R). This is the Alias key and cert, as well as
/// another key ahnd credential we use for the trust quorum diffie hellman
/// exchange (TQDHE).
fn gen_alias_artifacts(
    cdi_l1: &CdiL1,
    cert_serial_number: &mut CertSerialNumber,
    deviceid_keypair: &Keypair,
    fwid: &[u8; 32],
    handoff: &Handoff,
) {
    let alias_okm = AliasOkm::from_cdi(&cdi_l1);
    let alias_keypair = Keypair::from(alias_okm.as_bytes());

    let alias_cert = AliasCertBuilder::new(
        &cert_serial_number.next_num(),
        &alias_keypair.public,
        fwid,
    )
    .sign(&deviceid_keypair);

    let tqdhe_okm = TrustQuorumDheOkm::from_cdi(&cdi_l1);
    let tqdhe_keypair = Keypair::from(tqdhe_okm.as_bytes());

    let tqdhe_cert = TrustQuorumDheCertBuilder::new(
        &cert_serial_number.next_num(),
        &tqdhe_keypair.public,
        fwid,
    )
    .sign(&deviceid_keypair);

    let alias_data =
        AliasData::new(alias_okm, alias_cert, tqdhe_okm, tqdhe_cert);

    handoff.store(&alias_data);
}

/// Generate DICE keys and credentials that we pass to the Hubris task with
/// control over the SP (the next link in the measurement chain).
fn gen_spmeasure_artifacts(
    cdi_l1: &CdiL1,
    cert_serial_number: &mut CertSerialNumber,
    deviceid_keypair: &Keypair,
    fwid: &[u8; 32],
    handoff: &Handoff,
) {
    let spmeasure_okm = SpMeasureOkm::from_cdi(&cdi_l1);
    let spmeasure_keypair = Keypair::from(spmeasure_okm.as_bytes());

    let spmeasure_cert = SpMeasureCertBuilder::new(
        &cert_serial_number.next_num(),
        &spmeasure_keypair.public,
        fwid,
    )
    .sign(&deviceid_keypair);

    let spmeasure_data = SpMeasureData::new(spmeasure_okm, spmeasure_cert);

    handoff.store(&spmeasure_data);
}

/// Generate seed for the RNG task to seed its RNG.
fn gen_rng_artifacts(cdi_l1: &CdiL1, handoff: &Handoff) {
    let rng_seed = RngSeed::from_cdi(cdi_l1);
    let rng_data = RngData::new(rng_seed);

    handoff.store(&rng_data);
}

// Note: the inline(never) here is to keep this routine's stack usage, which
// will contain sensitive material, from commingling with the caller. Do not
// remove it without reconsidering our stack zeroization approach.
#[inline(never)]
pub fn run(handoff: &Handoff, peripherals: &Peripherals, flash: &mut Flash) {
    // The memory we use to handoff DICE artifacts is already enabled
    // in `main()`;

    // We get the CDI before mfg data to ensure that DICE is enabled. If
    // DICE has not been enabled we shouldn't do the mfg flows, but also
    // the PUF probably hasn't initialized by the ROM.
    let cdi = match Cdi::from_reg(&peripherals.SYSCON) {
        Some(cdi) => cdi,
        None => return,
    };

    let mut mfg_data = gen_mfg_artifacts(&peripherals, flash);

    let deviceid_keypair = gen_deviceid_artifacts(
        &cdi,
        &mut mfg_data.cert_serial_number,
        &mfg_data.platform_id,
        mfg_data.persistid_keypair,
        mfg_data.persistid_cert,
        mfg_data.intermediate_cert,
        handoff,
    );

    // The data we generate has a slot for "firmware ID." This made more sense
    // when this code and the Hubris image were distributed and signed
    // separately, and may begin making sense again when we reimplement the
    // trustzone split and need to attest to the nonsecure code we're about to
    // boot.
    //
    // However, for now, it is moot, and we've agreed to zero it as an
    // indication of that.
    let fwid = [0; 32];

    // create CDI for layer 1 (L1) firmware (the hubris image we're booting)
    let cdi_l1 = CdiL1::new(&cdi, &fwid);

    gen_alias_artifacts(
        &cdi_l1,
        &mut mfg_data.cert_serial_number,
        &deviceid_keypair,
        &fwid,
        handoff,
    );

    gen_spmeasure_artifacts(
        &cdi_l1,
        &mut mfg_data.cert_serial_number,
        &deviceid_keypair,
        &fwid,
        handoff,
    );

    gen_rng_artifacts(&cdi_l1, handoff);
}
