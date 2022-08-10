// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Vsc7448Rw, VscError};
use vsc7448_pac::*;
use vsc85xx::PhyRw;

/// This represents a PHY controlled through the VSC7448's built-in MIIM
/// controller.  It is a transient object that simply couples the main Vsc7448
/// object with a MIIM ID.
///
/// The user must configure GPIOs for proper MIIM operation beforehand.
pub struct Vsc7448MiimPhy<'a, R> {
    vsc7448: &'a R,
    miim: u8,
}

impl<'a, R: Vsc7448Rw> Vsc7448MiimPhy<'a, R> {
    pub fn new(vsc7448: &'a R, miim: u8) -> Self {
        Self { vsc7448, miim }
    }
    /// Builds a MII_CMD register based on the given phy and register.  Note
    /// that miim_cmd_opr_field is unset; you must configure it for a read
    /// or write yourself.
    fn miim_cmd(
        phy: u8,
        reg_addr: u8,
    ) -> vsc7448_pac::devcpu_gcb::miim::MII_CMD {
        let mut v: vsc7448_pac::devcpu_gcb::miim::MII_CMD = 0.into();
        v.set_miim_cmd_vld(1);
        v.set_miim_cmd_phyad(phy as u32);
        v.set_miim_cmd_regad(reg_addr as u32);
        v
    }

    /// Waits for the PENDING_RD and PENDING_WR bits to go low, indicating that
    /// it's safe to read or write to the MIIM.
    fn miim_idle_wait(&self) -> Result<(), VscError> {
        for _i in 0..32 {
            let status = self
                .vsc7448
                .read(DEVCPU_GCB().MIIM(self.miim).MII_STATUS())?;
            if status.miim_stat_opr_pend() == 0 {
                return Ok(());
            }
        }
        Err(VscError::MiimIdleTimeout)
    }

    /// Waits for the STAT_BUSY bit to go low, indicating that a read has
    /// finished and data is available.
    fn miim_read_wait(&self) -> Result<(), VscError> {
        for _i in 0..32 {
            let status = self
                .vsc7448
                .read(DEVCPU_GCB().MIIM(self.miim).MII_STATUS())?;
            if status.miim_stat_busy() == 0 {
                return Ok(());
            }
        }
        Err(VscError::MiimReadTimeout)
    }
}

impl<R: Vsc7448Rw> PhyRw for Vsc7448MiimPhy<'_, R> {
    fn read_raw(&self, phy: u8, reg: u8) -> Result<u16, VscError> {
        let mut v = Self::miim_cmd(phy, reg);
        v.set_miim_cmd_opr_field(0b10); // read

        self.miim_idle_wait()?;
        self.vsc7448
            .write(DEVCPU_GCB().MIIM(self.miim).MII_CMD(), v)?;
        self.miim_read_wait()?;

        let out = self.vsc7448.read(DEVCPU_GCB().MIIM(self.miim).MII_DATA())?;
        if out.miim_data_success() == 0b11 {
            return Err(VscError::MiimReadErr {
                miim: self.miim,
                phy,
                addr: reg,
            });
        }

        Ok(out.miim_data_rddata() as u16)
    }

    fn write_raw(&self, phy: u8, reg: u8, value: u16) -> Result<(), VscError> {
        let mut v = Self::miim_cmd(phy, reg);
        v.set_miim_cmd_opr_field(0b01); // read
        v.set_miim_cmd_wrdata(value as u32);

        self.miim_idle_wait()?;
        self.vsc7448
            .write(DEVCPU_GCB().MIIM(self.miim).MII_CMD(), v)
    }
}
