// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Vsc7448Rw, VscError};
use vsc7448_pac::*;

pub enum Mode {
    Sgmii,
}
pub struct Config {
    ob_amp_ctrl: u32,
    ob_idle: u32,
    cmv_term: u32,
    test_mode: u32,
    test_pattern: u32,
    mode_100fx: bool,
    ena_dc_coupling: bool,
    des_phs_ctrl: u32,
    cpmd_sel: u32,
    mbtr_ctrl: u32,
    des_bw_ana: u32,
    ena_lane: bool,
}

/// This controls how many times we poll the SERDES1 register after a
/// read/write operation before returning a timeout error.  The SDK polls
/// _forever_, which seems questionable, and has no pauses between polling.
const SERDES1G_RW_POLL_COUNT: usize = 32;

/// Writes to a specific SERDES1G instance, which is done by writing its
/// value (as a bitmask) to a particular register with a read flag set,
/// then waiting for the flag to autoclear.
pub fn serdes1g_read(v: &impl Vsc7448Rw, instance: u8) -> Result<(), VscError> {
    let addr = HSIO().MCB_SERDES1G_CFG().MCB_SERDES1G_ADDR_CFG();
    v.write_with(addr, |r| {
        r.set_serdes1g_rd_one_shot(1);
        r.set_serdes1g_addr(1 << instance);
    })?;
    for _ in 0..SERDES1G_RW_POLL_COUNT {
        if v.read(addr)?.serdes1g_rd_one_shot() != 1 {
            return Ok(());
        }
    }
    Err(VscError::Serdes1gReadTimeout { instance })
}

/// Reads from a specific SERDES1G instance, which is done by writing its
/// value (as a bitmask) to a particular register with a read flag set,
/// then waiting for the flag to autoclear.
pub fn serdes1g_write(
    v: &impl Vsc7448Rw,
    instance: u8,
) -> Result<(), VscError> {
    let addr = HSIO().MCB_SERDES1G_CFG().MCB_SERDES1G_ADDR_CFG();
    v.write_with(addr, |r| {
        r.set_serdes1g_wr_one_shot(1);
        r.set_serdes1g_addr(1 << instance);
    })?;
    for _ in 0..SERDES1G_RW_POLL_COUNT {
        if v.read(addr)?.serdes1g_wr_one_shot() != 1 {
            return Ok(());
        }
    }
    Err(VscError::Serdes1gWriteTimeout { instance })
}

/// Based on `jr2_sd1g_cfg` in the MESA SDK
impl Config {
    pub fn new(m: Mode) -> Self {
        match m {
            Mode::Sgmii => Self {
                ob_amp_ctrl: 12,
                ob_idle: 0,
                cmv_term: 1,
                test_mode: 0,
                test_pattern: 0,
                mode_100fx: false,
                ena_dc_coupling: false,
                des_phs_ctrl: 6,
                cpmd_sel: 0,
                mbtr_ctrl: 2,
                des_bw_ana: 6,
                ena_lane: true,
            },
        }
    }
    pub fn apply(
        &self,
        instance: u8,
        v: &impl Vsc7448Rw,
    ) -> Result<(), VscError> {
        serdes1g_read(v, instance)?;
        let ana_cfg = HSIO().SERDES1G_ANA_CFG();
        let dig_cfg = HSIO().SERDES1G_DIG_CFG();
        v.modify(ana_cfg.SERDES1G_SER_CFG(), |r| {
            r.set_ser_idle(self.ob_idle);
        })?;
        v.write(dig_cfg.SERDES1G_TP_CFG(), self.test_pattern.into())?;
        v.modify(dig_cfg.SERDES1G_DFT_CFG0(), |r| {
            r.set_test_mode(self.test_mode);
        })?;
        v.modify(ana_cfg.SERDES1G_OB_CFG(), |r| {
            r.set_ob_amp_ctrl(self.ob_amp_ctrl);
        })?;
        v.modify(ana_cfg.SERDES1G_IB_CFG(), |r| {
            r.set_ib_ena_cmv_term(self.cmv_term);
            r.set_ib_fx100_ena(self.mode_100fx.into());
            r.set_ib_ena_dc_coupling(self.ena_dc_coupling.into());
            r.set_ib_resistor_ctrl(13);
        })?;
        v.modify(ana_cfg.SERDES1G_DES_CFG(), |r| {
            r.set_des_phs_ctrl(self.des_phs_ctrl);
            r.set_des_cpmd_sel(self.cpmd_sel);
            r.set_des_mbtr_ctrl(self.mbtr_ctrl);
            r.set_des_bw_ana(self.des_bw_ana);
        })?;
        v.modify(dig_cfg.SERDES1G_MISC_CFG(), |r| {
            r.set_des_100fx_cpmd_ena(self.mode_100fx.into());
            r.set_lane_rst(1);
        })?;
        v.modify(ana_cfg.SERDES1G_PLL_CFG(), |r| {
            r.set_pll_fsm_ena(1);
        })?;
        v.modify(ana_cfg.SERDES1G_COMMON_CFG(), |r| {
            r.set_ena_lane(self.ena_lane.into());
        })?;
        serdes1g_write(v, instance)?;

        v.modify(ana_cfg.SERDES1G_COMMON_CFG(), |r| {
            r.set_sys_rst(1);
        })?;
        serdes1g_write(v, instance)?;

        v.modify(dig_cfg.SERDES1G_MISC_CFG(), |r| {
            r.set_lane_rst(0);
        })?;
        serdes1g_write(v, instance)?;

        Ok(())
    }

    /// Brings down the given SERDES by enabling IDLE mode
    pub fn disable_output(
        instance: u8,
        v: &impl Vsc7448Rw,
    ) -> Result<(), VscError> {
        serdes1g_read(v, instance)?;
        let ana_cfg = HSIO().SERDES1G_ANA_CFG();
        v.modify(ana_cfg.SERDES1G_SER_CFG(), |r| {
            r.set_ser_idle(1);
        })?;
        serdes1g_write(v, instance)?;
        Ok(())
    }
}
