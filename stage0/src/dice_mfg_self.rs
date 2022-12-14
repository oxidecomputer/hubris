// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::dice::SerialNumbers;
use crate::Handoff;
use dice_crate::{CertData, DeviceIdSelfMfg, DiceMfg};
use salty::signature::Keypair;

pub fn gen_mfg_artifacts(
    deviceid_keypair: &Keypair,
    handoff: &Handoff,
) -> SerialNumbers {
    let mfg_state = DeviceIdSelfMfg::new(&deviceid_keypair).run();

    // transfer certs to CertData for serialization
    let cert_data =
        CertData::new(mfg_state.deviceid_cert, mfg_state.intermediate_cert);

    handoff.store(&cert_data);

    // transfer platform and cert serial number to structure & return
    SerialNumbers {
        cert_serial_number: mfg_state.cert_serial_number,
        serial_number: mfg_state.serial_number,
    }
}
