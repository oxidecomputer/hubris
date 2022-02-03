// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{spi::Vsc7448Spi, VscError};
use vsc7448_pac::Vsc7448;

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
    pub fn apply(&self, instance: u32, v: &Vsc7448Spi) -> Result<(), VscError> {
        v.serdes1g_read(instance)?;
        let ana_cfg = Vsc7448::HSIO().SERDES1G_ANA_CFG();
        let dig_cfg = Vsc7448::HSIO().SERDES1G_DIG_CFG();
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
        v.serdes1g_write(instance)?;

        v.modify(ana_cfg.SERDES1G_COMMON_CFG(), |r| {
            r.set_sys_rst(1);
        })?;
        v.serdes1g_write(instance)?;

        v.modify(dig_cfg.SERDES1G_MISC_CFG(), |r| {
            r.set_lane_rst(0);
        })?;
        v.serdes1g_write(instance)?;

        Ok(())
    }
}
