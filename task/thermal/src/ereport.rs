// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::i2c_config::MAX_COMPONENT_ID_LEN;

#[derive(microcbor::Encode)]
#[cbor(variant_id = "k")]
pub enum Ereport {
    #[cbor(rename = "hw.temp.crit")]
    TempCritical {
        refdes: FixedStr<MAX_COMPONENT_ID_LEN>,
    },
}
