// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Deserialize;

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct DataRegion {
    pub address: u32,
    pub size: u32,
}
