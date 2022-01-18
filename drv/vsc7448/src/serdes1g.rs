// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{spi::Vsc7448Spi, VscError};

pub enum Mode {
    Sgmii,
}
pub struct Config {
    // Nothing in here
}
impl Config {
    pub fn new(m: Mode) -> Self {
        unimplemented!()
    }
    pub fn apply(&self, instance: u32, v: &Vsc7448Spi) -> Result<(), VscError> {
        unimplemented!()
    }
}
