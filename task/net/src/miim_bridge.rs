// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
use drv_stm32h7_eth as eth;
use vsc7448_pac::types::PhyRegisterAddress;
use vsc85xx::{PhyRw, VscError};

/// Helper struct to implement the `PhyRw` trait using direct access through
/// `eth`'s MIIM registers.  This allows us to use functions that expect
/// that trait, e.g. VSC85xx PHY initialization.
pub struct MiimBridge<'a> {
    eth: &'a mut eth::Ethernet,
}

impl<'a> MiimBridge<'a> {
    pub fn new(eth: &'a mut eth::Ethernet) -> Self {
        Self { eth }
    }
}

impl PhyRw for MiimBridge<'_> {
    fn read_raw<T: From<u16>>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        Ok(self.eth.smi_read(phy, reg.addr).into())
    }
    fn write_raw<T>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone,
    {
        self.eth.smi_write(phy, reg.addr, value.into());
        Ok(())
    }
}
