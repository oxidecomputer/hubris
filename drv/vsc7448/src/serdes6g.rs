use crate::{spi::Vsc7448Spi, VscError};
use userlib::hl;
use vsc7448_pac::Vsc7448;

pub enum Mode {
    Sgmii,
    Qsgmii,
}

// ob_ena_cas, ob_lev, qrate, des_ana_bw
pub struct Config {
    ob_ena1v_mode: u32,
    ob_ena_cas: u32,
    ob_lev: u32,
    pll_fsm_ctrl_data: u32,
    qrate: u32,
    if_mode: u32,
    des_bw_ana: u32,
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
            },
            Mode::Sgmii => Self {
                ob_ena1v_mode: 1,
                ob_ena_cas: 2,
                ob_lev: 48,
                pll_fsm_ctrl_data: 60,
                qrate: 1,
                if_mode: 1,
                des_bw_ana: 3,
            },
        }
    }

    pub fn apply(&self, instance: u32, v: &Vsc7448Spi) -> Result<(), VscError> {
        v.serdes6g_read(instance)?;
        let ana_cfg = Vsc7448::HSIO().SERDES6G_ANA_CFG();
        let dig_cfg = Vsc7448::HSIO().SERDES6G_DIG_CFG();
        v.modify(ana_cfg.SERDES6G_COMMON_CFG(), |r| {
            r.set_sys_rst(0);
        })?;
        v.modify(dig_cfg.SERDES6G_MISC_CFG(), |r| {
            r.set_lane_rst(1);
        })?;
        v.serdes6g_write(instance)?;

        v.modify(ana_cfg.SERDES6G_OB_CFG(), |r| {
            r.set_ob_ena1v_mode(self.ob_ena1v_mode);
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
        v.serdes6g_write(instance)?;

        // Enable the PLL then wait 20 ms for bringup
        v.modify(ana_cfg.SERDES6G_PLL_CFG(), |r| r.set_pll_fsm_ena(1))?;
        v.serdes6g_write(instance)?;
        hl::sleep_for(20);

        // Start IB calibration, then wait 60 ms for it to complete
        v.modify(ana_cfg.SERDES6G_IB_CFG(), |r| r.set_ib_cal_ena(1))?;
        v.modify(dig_cfg.SERDES6G_MISC_CFG(), |r| r.set_lane_rst(0))?;
        v.serdes6g_write(instance)?;
        hl::sleep_for(60);

        // "Set ib_tsdet and ib_reg_pat_sel_offset back to correct value"
        // (according to the SDK)
        v.modify(ana_cfg.SERDES6G_IB_CFG(), |r| {
            r.set_ib_reg_pat_sel_offset(0);
            r.set_ib_sig_det_clk_sel(7);
        })?;
        v.modify(ana_cfg.SERDES6G_IB_CFG1(), |r| r.set_ib_tsdet(3))?;
        v.serdes6g_write(instance)?;

        Ok(())
    }
}
