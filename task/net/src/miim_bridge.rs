// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
use drv_stm32h7_eth as eth;
use vsc85xx::{PhyRw, VscError};

/// Helper struct to implement the `PhyRw` trait using direct access through
/// `eth`'s MIIM registers.  This allows us to use functions that expect
/// that trait, e.g. VSC85xx PHY initialization.
pub struct MiimBridge<'a> {
    eth: &'a eth::Ethernet,
}

impl<'a> MiimBridge<'a> {
    #[allow(dead_code)]
    pub fn new(eth: &'a eth::Ethernet) -> Self {
        Self { eth }
    }
}

impl PhyRw for MiimBridge<'_> {
    #[inline(always)]
    fn read_raw(&self, phy: u8, reg: u8) -> Result<u16, VscError> {
        Ok(self.eth.smi_read(phy, reg))
    }

    #[inline(always)]
    fn write_raw(&self, phy: u8, reg: u8, value: u16) -> Result<(), VscError> {
        self.eth.smi_write(phy, reg, value);
        Ok(())
    }
}
