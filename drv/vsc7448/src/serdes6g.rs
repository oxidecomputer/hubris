// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Vsc7448Rw, VscError};
use userlib::hl;
use vsc7448_pac::*;

pub enum Mode {
    Sgmii,
    Qsgmii,
}

// ob_ena_cas, ob_lev, qrate, des_ana_bw
pub struct Config {
    ob_ena1v_mode: u32,
    ob_ena_cas: u32,
    ob_lev: u32,
    ob_sr_h: u32,
    ob_sr: u32,
    pll_fsm_ctrl_data: u32,
    qrate: u32,
    if_mode: u32,
    des_bw_ana: u32,
}

/// This controls how many times we poll the SERDES1 register after a
/// read/write operation before returning a timeout error.  The SDK polls
/// _forever_, which seems questionable, and has no pauses between polling.
const SERDES6G_RW_POLL_COUNT: usize = 32;

/// Reads from a specific SERDES6G instance, which is done by writing its
/// value (as a bitmask) to a particular register with a read flag set,
/// then waiting for the flag to autoclear.
pub fn serdes6g_read(v: &impl Vsc7448Rw, instance: u8) -> Result<(), VscError> {
    let addr = HSIO().MCB_SERDES6G_CFG().MCB_SERDES6G_ADDR_CFG();
    v.write_with(addr, |r| {
        r.set_serdes6g_rd_one_shot(1);
        r.set_serdes6g_addr(1 << instance);
    })?;
    // TODO: look at whether this ever takes more than one iteration.
    // (same for other instances in this file)
    for _ in 0..SERDES6G_RW_POLL_COUNT {
        if v.read(addr)?.serdes6g_rd_one_shot() != 1 {
            return Ok(());
        }
    }
    Err(VscError::Serdes6gReadTimeout { instance })
}

/// Writes to a specific SERDES6G instance, which is done by writing its
/// value (as a bitmask) to a particular register with a read flag set,
/// then waiting for the flag to autoclear.
pub fn serdes6g_write(
    v: &impl Vsc7448Rw,
    instance: u8,
) -> Result<(), VscError> {
    let addr = HSIO().MCB_SERDES6G_CFG().MCB_SERDES6G_ADDR_CFG();
    v.write_with(addr, |r| {
        r.set_serdes6g_wr_one_shot(1);
        r.set_serdes6g_addr(1 << instance);
    })?;
    for _ in 0..SERDES6G_RW_POLL_COUNT {
        if v.read(addr)?.serdes6g_wr_one_shot() != 1 {
            return Ok(());
        }
    }
    Err(VscError::Serdes6gWriteTimeout { instance })
}

// Based on the beginning of `jr2_sd6g_cfg`, with only relevant parameters
// (i.e. those that differ from reset and between modes) broken out
impl Config {
    pub fn new(mode: Mode) -> Self {
        match mode {
            Mode::Qsgmii => Self {
                ob_ena1v_mode: 0,
                ob_ena_cas: 0,
                ob_lev: 24,
                pll_fsm_ctrl_data: 120,
                qrate: 0,
                if_mode: 3,
                des_bw_ana: 5,

                // This output buffer config isn't part of `jr2_sd6g_cfg`, but
                // checking experimentally on the scope, it makes the QSGMII
                // edges look much better.
                ob_sr_h: 0,
                ob_sr: 0,
            },
            Mode::Sgmii => Self {
                ob_ena1v_mode: 1,
                ob_ena_cas: 2,
                ob_lev: 48,
                pll_fsm_ctrl_data: 60,
                qrate: 1,
                if_mode: 1,
                des_bw_ana: 3,
                ob_sr_h: 1,
                ob_sr: 7,
            },
        }
    }

    pub fn apply(
        &self,
        instance: u8,
        v: &impl Vsc7448Rw,
    ) -> Result<(), VscError> {
        serdes6g_read(v, instance)?;
        let ana_cfg = HSIO().SERDES6G_ANA_CFG();
        let dig_cfg = HSIO().SERDES6G_DIG_CFG();
        v.modify(ana_cfg.SERDES6G_COMMON_CFG(), |r| {
            r.set_sys_rst(0);
        })?;
        v.modify(dig_cfg.SERDES6G_MISC_CFG(), |r| {
            r.set_lane_rst(1);
        })?;
        serdes6g_write(v, instance)?;

        v.modify(ana_cfg.SERDES6G_OB_CFG(), |r| {
            r.set_ob_ena1v_mode(self.ob_ena1v_mode);
            r.set_ob_sr_h(self.ob_sr_h);
            r.set_ob_sr(self.ob_sr);
        })?;
        v.modify(ana_cfg.SERDES6G_OB_CFG1(), |r| {
            r.set_ob_ena_cas(self.ob_ena_cas);
        })?;
        v.modify(ana_cfg.SERDES6G_OB_CFG1(), |r| {
            r.set_ob_lev(self.ob_lev);
        })?;
        v.modify(ana_cfg.SERDES6G_DES_CFG(), |r| {
            r.set_des_bw_ana(self.des_bw_ana);
        })?;
        v.modify(ana_cfg.SERDES6G_IB_CFG(), |r| {
            r.set_ib_reg_pat_sel_offset(0)
        })?;
        // Skip configuration related to VTSS_PORT_LB_FACILITY/EQUIPMENT
        v.modify(ana_cfg.SERDES6G_PLL_CFG(), |r| {
            r.set_pll_fsm_ctrl_data(self.pll_fsm_ctrl_data);
        })?;
        v.modify(ana_cfg.SERDES6G_COMMON_CFG(), |r| {
            r.set_sys_rst(1);
            r.set_ena_lane(1);
            r.set_qrate(self.qrate);
            r.set_if_mode(self.if_mode);
        })?;
        serdes6g_write(v, instance)?;

        // Enable the PLL then wait 20 ms for bringup
        v.modify(ana_cfg.SERDES6G_PLL_CFG(), |r| r.set_pll_fsm_ena(1))?;
        serdes6g_write(v, instance)?;
        hl::sleep_for(20);

        // Start IB calibration, then wait 60 ms for it to complete
        v.modify(ana_cfg.SERDES6G_IB_CFG(), |r| r.set_ib_cal_ena(1))?;
        v.modify(dig_cfg.SERDES6G_MISC_CFG(), |r| r.set_lane_rst(0))?;
        serdes6g_write(v, instance)?;
        hl::sleep_for(60);

        // "Set ib_tsdet and ib_reg_pat_sel_offset back to correct value"
        // (according to the SDK)
        v.modify(ana_cfg.SERDES6G_IB_CFG(), |r| {
            r.set_ib_reg_pat_sel_offset(0);
            r.set_ib_sig_det_clk_sel(7);
        })?;
        v.modify(ana_cfg.SERDES6G_IB_CFG1(), |r| r.set_ib_tsdet(3))?;
        serdes6g_write(v, instance)?;

        Ok(())
    }
}
