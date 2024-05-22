// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#[derive(serde::Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
#[cfg(feature = "dice-seed")]
pub struct DataRegion {
    pub address: usize,
    pub size: usize,
}
