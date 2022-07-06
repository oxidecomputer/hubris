// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// Tools for working with the 10G SERDES (sd10g65 in the SDK)
use crate::{Vsc7448Rw, VscError};
use userlib::hl;
use vsc7448_pac::*;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Mode {
    Lan10g,
    Sgmii,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Config {
    f_pll_khz_plain: u32,

    mult: SynthMultCalc,
    rx_preset: SerdesRxPreset,
    apc_preset: SerdesApcPreset,
    if_width: u32,
    if_mode_sel: u32,
    ob_cfg2_d_filter: u32,

    half_rate_mode: bool,
    high_data_rate: bool,
    optimize_for_1g: bool,
    tx_synth_off_comp_ena: u32,
    pll_lpf_cur: u32,
    pll_lpf_res: u32,
    pllf_ref_cnt_end: u32,

    mode: Mode,
}

impl Config {
    pub fn new(mode: Mode) -> Result<Self, VscError> {
        let mut f_pll = FrequencySetup::new(mode);
        let if_width = match mode {
            Mode::Lan10g => 32,
            Mode::Sgmii => 10,
        };

        // `sd10g65_get_iw_setting`
        let if_mode_sel = match if_width {
            8 => 0,
            10 => 1,
            16 => 2,
            20 => 3,
            32 => 4,
            40 => 5,
            _ => panic!("Invalid if_width {}", if_width),
        };

        // `vtss_calc_sd10g65_setup_tx`
        let mut f_pll_khz_plain =
            ((f_pll.f_pll_khz as u64 * f_pll.ratio_num as u64)
                / (f_pll.ratio_den as u64)) as u32;

        // These two values are set in `vtss_jaguar2c_apc10g_setup`, so I think
        // they're using the unscaled values (i.e. the half_rate_mode adjustment
        // isn't applied to them)
        let optimize_for_1g = f_pll.f_pll_khz <= 2_500_000;

        // XXX: What happens if this is exactly 2_500_000?  Then half_rate_mode
        // will be false and high_data_rate will also be false.
        let high_data_rate = f_pll_khz_plain > 2_500_000;

        let half_rate_mode = if f_pll_khz_plain < 2_500_000 {
            f_pll_khz_plain *= 2;
            f_pll.f_pll_khz *= 2;
            true
        } else {
            false
        };

        let mult = SynthMultCalc::new(&f_pll)?;

        let ob_cfg2_d_filter = if if_width > 10 { 0x7DF820 } else { 0x820820 };

        let tx_synth_off_comp_ena =
            if f_pll_khz_plain > 10_312_500 { 31 } else { 23 };

        let (pll_lpf_cur, pll_lpf_res) = if f_pll_khz_plain > 7_000_000 {
            (3, 10)
        } else if f_pll_khz_plain > 3_000_000 {
            (2, 15)
        } else {
            (0, 10)
        };

        let pllf_ref_cnt_end = if half_rate_mode {
            (if_width * 64 * 1000000) / (f_pll_khz_plain >> 1)
        } else {
            (if_width * 64 * 1000000) / f_pll_khz_plain
        };

        ////////////////////////////////////////////////////////////////////////
        // `vtss_calc_sd10g65_setup_rx`
        let preset_type = SerdesPresetType::DacHw;
        let rx_preset = SerdesRxPreset::new(preset_type);
        let apc_preset = SerdesApcPreset::new(preset_type, optimize_for_1g);

        Ok(Self {
            f_pll_khz_plain,
            mult,
            rx_preset,
            apc_preset,
            if_width,
            if_mode_sel,
            half_rate_mode,
            high_data_rate,
            optimize_for_1g,
            tx_synth_off_comp_ena,
            pll_lpf_cur,
            pll_lpf_res,
            pllf_ref_cnt_end,
            ob_cfg2_d_filter,
            mode,
        })
    }
    /// Based on `jaguar2c_sd10g_*_register_cfg`.  Any variables which aren't
    /// changed are converted into direct register assignments (rather than
    /// passing them around in the config struct).
    pub fn apply(&self, index: u8, v: &impl Vsc7448Rw) -> Result<(), VscError> {
        // jr2_sd10g_xfi_mode
        // "Set XFI to default"
        v.write(XGXFI(index).XFI_CONTROL().XFI_MODE(), 5.into())?;

        // Select either the 40-bit port (for SGMII) or 64-bit (for SFI)
        v.modify(XGXFI(index).XFI_CONTROL().XFI_MODE(), |r| {
            r.set_port_sel(match self.mode {
                Mode::Sgmii => 1,
                Mode::Lan10g => 0,
            })
        })?;
        // Unclear if these all need to be in separate messages, but let's
        // match the SDK behavior exactly for now.
        v.modify(XGXFI(index).XFI_CONTROL().XFI_MODE(), |r| {
            r.set_sw_rst(0);
        })?;
        v.modify(XGXFI(index).XFI_CONTROL().XFI_MODE(), |r| {
            r.set_sw_ena(1);
        })?;
        v.modify(XGXFI(index).XFI_CONTROL().XFI_MODE(), |r| {
            r.set_endian(1);
        })?;
        v.modify(XGXFI(index).XFI_CONTROL().XFI_MODE(), |r| {
            r.set_tx_resync_shot(1);
        })?;

        let dev = XGANA(index);

        ////////////////////////////////////////////////////////////////////////
        //  `jaguar2c_sd10g_tx_register_cfg`
        let tx_synth = dev.SD10G65_TX_SYNTH();
        let tx_rcpll = dev.SD10G65_TX_RCPLL();
        let ob = dev.SD10G65_OB();
        let ib = dev.SD10G65_IB();
        let des = dev.SD10G65_DES();

        v.modify(ob.SD10G65_SBUS_TX_CFG(), |r| {
            r.set_sbus_bias_en(1);

            // This is theoretically the default value, but experimentally the
            // chip comes out of reset with SBUS_BIAS_SPEED_SEL = 0, so we
            // set it here.
            r.set_sbus_bias_speed_sel(3);
        })?;

        // Quoth the datasheet, "Note: SBUS configuration applies for RX/TX
        // aggregates only, any configuration applied to SBUS_TX_CFG (output
        // buffer cfg space) will be ignored."
        //
        // I think this means we need to configure both SD10G65_SBUS_TX_CFG and
        // SD10G65_SBUS_RX_CFG here; otherwise, the Tx PLL won't lock.
        v.modify(ib.SD10G65_SBUS_RX_CFG(), |r| {
            r.set_sbus_bias_en(1);
            r.set_sbus_bias_speed_sel(3);
        })?;

        v.modify(ob.SD10G65_OB_CFG0(), |r| {
            r.set_en_ob(1);
        })?;
        v.modify(tx_rcpll.SD10G65_TX_RCPLL_CFG2(), |r| {
            r.set_pll_ena(1);
        })?;

        // These need to be separate read-modify-write operations, despite
        // touching the same register; otherwise, it doesn't work.
        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG0(), |r| {
            r.set_synth_ena(1);
        })?;
        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG0(), |r| {
            r.set_synth_spare_pool(7);
            r.set_synth_off_comp_ena(self.tx_synth_off_comp_ena);
        })?;
        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG0(), |r| {
            r.set_synth_speed_sel(self.mult.speed_sel.into());
            r.set_synth_fbdiv_sel(self.mult.fbdiv_sel.into());
        })?;

        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG3(), |r| {
            r.set_synth_freqm_0((self.mult.settings.freqm & 0xFFFFFFFF) as u32);
        })?;
        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG4(), |r| {
            r.set_synth_freqn_0((self.mult.settings.freqn & 0xFFFFFFFF) as u32);
        })?;
        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG1(), |r| {
            r.set_synth_freq_mult_byp(1);
            r.set_synth_freq_mult(self.mult.freq_mult_byp.freq_mult.into());
            r.set_synth_freq_mult_hi(
                self.mult.freq_mult_byp.freq_mult_hi as u32,
            );
            r.set_synth_freqm_1((self.mult.settings.freqm >> 32) as u32);
            r.set_synth_freqn_1((self.mult.settings.freqn >> 32) as u32);
        })?;
        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG0(), |r| {
            r.set_synth_ls_speed(self.mult.tx_ls_speed.into());
            r.set_synth_cs_speed(self.mult.tx_cs_speed.into());
        })?;
        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG0(), |r| {
            r.set_synth_hrate_ena(self.half_rate_mode.into());
            // These aren't in the datasheet, but are copied from the SDK
            r.set_synth_ena_sync_unit(1);
            r.set_synth_conv_ena(1);
            r.set_synth_ds_dir(0);
            r.set_synth_ds_speed(0);
            r.set_synth_ls_dir(0);
            r.set_synth_ls_ena(0);
        })?;
        v.modify(tx_synth.SD10G65_SSC_CFG1(), |r| {
            r.set_sync_ctrl_fsel(35);
        })?;

        v.modify(ob.SD10G65_OB_CFG0(), |r| {
            r.set_sel_ifw(self.if_mode_sel);
        })?;
        // The SDK also writes to OB_CFG1, but only to set defaults

        v.modify(ob.SD10G65_OB_CFG2(), |r| {
            r.set_d_filter(self.ob_cfg2_d_filter);
        })?;

        v.modify(tx_rcpll.SD10G65_TX_RCPLL_CFG2(), |r| {
            r.set_pll_vco_cur(7);
            r.set_pll_vreg18(10);
            r.set_pll_lpf_cur(self.pll_lpf_cur);
            r.set_pll_lpf_res(self.pll_lpf_res);
        })?;
        v.modify(tx_rcpll.SD10G65_TX_RCPLL_CFG0(), |r| {
            // These also aren't in the datasheet; values are from the SDK
            r.set_pllf_syn_clk_ena(0);
            r.set_pllf_loop_ctrl_ena(0);
            r.set_pllf_loop_ena(0);
        })?;
        v.modify(tx_rcpll.SD10G65_TX_RCPLL_CFG1(), |r| {
            r.set_pllf_ref_cnt_end(self.pllf_ref_cnt_end);
        })?;
        v.modify(tx_rcpll.SD10G65_TX_RCPLL_CFG0(), |r| {
            r.set_pllf_oor_recal_ena(1);
        })?;

        hl::sleep_for(10);
        v.modify(tx_rcpll.SD10G65_TX_RCPLL_CFG0(), |r| {
            r.set_pllf_ena(1);
        })?;
        v.modify(tx_rcpll.SD10G65_TX_RCPLL_CFG0(), |r| {
            r.set_pllf_oor_recal_ena(0);
        })?;

        hl::sleep_for(2);

        let stat0 = v.read(tx_rcpll.SD10G65_TX_RCPLL_STAT0())?;
        if stat0.pllf_lock_stat() != 1 {
            return Err(VscError::TxPllLockFailed);
        }
        let stat1 = v.read(tx_rcpll.SD10G65_TX_RCPLL_STAT1())?;
        if stat1.pllf_fsm_stat() != 13 {
            return Err(VscError::TxPllFsmFailed);
        }

        ////////////////////////////////////////////////////////////////////////
        //  `jaguar2c_sd10g_rx_register_cfg`

        let rx_rcpll = dev.SD10G65_RX_RCPLL();
        v.modify(rx_rcpll.SD10G65_RX_RCPLL_CFG2(), |r| {
            r.set_pll_ena(1);
        })?;

        let rx_synth = dev.SD10G65_RX_SYNTH();
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG0(), |r| {
            r.set_synth_ena(1);
        })?;
        v.modify(ib.SD10G65_SBUS_RX_CFG(), |r| {
            r.set_sbus_bias_en(1);
            r.set_sbus_spare_pool(0);
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG2(), |r| {
            r.set_synth_aux_ena(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG0(), |r| {
            r.set_ib_clkdiv_ena(1);
            r.set_ib_vbulk_sel(1);
            r.set_ib_sam_ena(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG8(), |r| {
            r.set_ib_bias_mode(1);
            r.set_ib_cml_curr(0);

            r.set_ib_bias_adj(self.rx_preset.ib_bias_adj.into());
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG0(), |r| {
            r.set_synth_spare_pool(7);
            r.set_synth_off_comp_ena(15);
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG0(), |r| {
            r.set_synth_speed_sel(self.mult.speed_sel.into());
            r.set_synth_fbdiv_sel(self.mult.fbdiv_sel.into());
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG3(), |r| {
            r.set_synth_freqm_0((self.mult.settings.freqm & 0xFFFFFFFF) as u32);
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG4(), |r| {
            r.set_synth_freqn_0((self.mult.settings.freqn & 0xFFFFFFFF) as u32);
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG1(), |r| {
            r.set_synth_freq_mult_byp(1);
            r.set_synth_freq_mult(self.mult.freq_mult_byp.freq_mult.into());
            r.set_synth_freq_mult_hi(
                self.mult.freq_mult_byp.freq_mult_hi as u32,
            );
            r.set_synth_freqm_1((self.mult.settings.freqm >> 32) as u32);
            r.set_synth_freqn_1((self.mult.settings.freqn >> 32) as u32);
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG0(), |r| {
            r.set_synth_fb_step(self.mult.rx_fb_step.into());
            r.set_synth_i2_step(self.mult.rx_i2_step.into());
        })?;

        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG0(), |r| {
            r.set_synth_hrate_ena(self.half_rate_mode.into());
            r.set_synth_i2_ena(1);
            r.set_synth_conv_ena(1);
        })?;

        v.modify(rx_synth.SD10G65_RX_SYNTH_SYNC_CTRL(), |r| {
            r.set_synth_sc_sync_timer_sel(0);
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG2(), |r| {
            r.set_synth_phase_data(self.rx_preset.synth_phase_data.into());
            r.set_synth_cpmd_dig_ena(0); // Not in MODE_FX100
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG0(), |r| {
            r.set_synth_p_step(1);
            r.set_synth_i1_step(1);
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG2(), |r| {
            // This intentionally assigns the same value to both I1E and I1M,
            // based on the preset configuration.
            r.set_synth_dv_ctrl_i1e(self.rx_preset.synth_dv_ctrl_i1e.into());
            r.set_synth_dv_ctrl_i1m(self.rx_preset.synth_dv_ctrl_i1e.into());
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CDRLF(), |r| {
            r.set_synth_integ1_max1(10);
            r.set_synth_integ1_max0(10);
            r.set_synth_integ1_lim(10);
            // Values from `vtss_sd10g65_setup_rx_args_init`
            r.set_synth_integ1_fsel(10);
            r.set_synth_integ2_fsel(35);
        })?;
        v.modify(ib.SD10G65_IB_CFG0(), |r| {
            r.set_ib_rib_adj(self.rx_preset.ib_rib_adj.into());
            r.set_ib_eqz_ena(1);
            r.set_ib_dfe_ena(1);
            r.set_ib_ld_ena(1);
            r.set_ib_ia_ena(1);
            r.set_ib_ia_sdet_ena(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG3(), |r| {
            r.set_ib_eq_ld1_offset(self.rx_preset.ib_eq_ld1_offset.into());
            r.set_ib_ldsd_divsel(0);
            r.set_ib_ia_sdet_level(2);
            r.set_ib_sdet_sel(0);
        })?;
        v.modify(ib.SD10G65_IB_CFG5(), |r| {
            r.set_ib_offs_value(31);
            r.set_ib_calmux_ena(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG6(), |r| {
            r.set_ib_sam_offs_adj(self.rx_preset.ib_sam_offs_adj.into());

            // Depends on chip family; our chip is a JAGUAR2C
            r.set_ib_auto_agc_adj(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG7(), |r| {
            r.set_ib_dfe_gain_adj_s(1);
            r.set_ib_dfe_gain_adj(self.rx_preset.ib_dfe_gain_adj.into());
            r.set_ib_dfe_offset_h(
                (4 + 19 * self.rx_preset.ib_vscope_hl_offs as u32) / 8,
            );
        })?;

        // In the SDK, this set-then-clear operation is called if skip_cal = 0
        // Based on the datasheet, this resets the latches.
        v.modify(ib.SD10G65_IB_CFG8(), |r| {
            r.set_ib_lat_neutral(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG8(), |r| {
            r.set_ib_lat_neutral(0);
        })?;
        v.modify(ib.SD10G65_IB_CFG4(), |r| {
            // This deliberately applies the same value across all adjustments
            r.set_ib_eqz_c_adj_ib(self.rx_preset.ib_eqz_c_adj.into());
            r.set_ib_eqz_c_adj_es0(self.rx_preset.ib_eqz_c_adj.into());
            r.set_ib_eqz_c_adj_es1(self.rx_preset.ib_eqz_c_adj.into());
            r.set_ib_eqz_c_adj_es2(self.rx_preset.ib_eqz_c_adj.into());
            r.set_ib_eqz_c_mode(self.rx_preset.ib_eqz_c_mode.into());
            r.set_ib_eqz_l_mode(self.rx_preset.ib_eqz_l_mode.into());
            r.set_ib_vscope_h_thres(
                (32 + self.rx_preset.ib_vscope_hl_offs).into(),
            );
            r.set_ib_vscope_l_thres(
                (31 - self.rx_preset.ib_vscope_hl_offs).into(),
            );
            r.set_ib_main_thres(
                (32 + self.rx_preset.ib_main_thres_offs).into(),
            );
        })?;
        v.modify(ib.SD10G65_IB_CFG11(), |r| {
            r.set_ib_ena_400_inp(self.rx_preset.ib_ena_400_inp.into());
            r.set_ib_tc_dfe(self.rx_preset.ib_tc_dfe.into());
            r.set_ib_tc_eq(self.rx_preset.ib_tc_eq.into());
        })?;
        v.modify(des.SD10G65_DES_CFG0(), |r| {
            r.set_des_if_mode_sel(self.if_mode_sel);
        })?;

        v.modify(rx_rcpll.SD10G65_RX_RCPLL_CFG2(), |r| {
            r.set_pll_vco_cur(7);
            r.set_pll_vreg18(10);
            r.set_pll_lpf_cur(self.pll_lpf_cur);
            r.set_pll_lpf_res(self.pll_lpf_res);
        })?;
        v.modify(rx_rcpll.SD10G65_RX_RCPLL_CFG0(), |r| {
            r.set_pllf_start_cnt(2);
            r.set_pllf_syn_clk_ena(0);
            r.set_pllf_loop_ctrl_ena(0);
            r.set_pllf_loop_ena(0);
            r.set_pllf_ena(0);
        })?;
        v.modify(rx_rcpll.SD10G65_RX_RCPLL_CFG1(), |r| {
            r.set_pllf_ref_cnt_end(self.pllf_ref_cnt_end);
        })?;
        v.modify(rx_rcpll.SD10G65_RX_RCPLL_CFG0(), |r| {
            r.set_pllf_oor_recal_ena(1);
        })?;
        v.modify(rx_rcpll.SD10G65_RX_RCPLL_CFG0(), |r| {
            r.set_pllf_ena(1);
        })?;
        v.modify(rx_rcpll.SD10G65_RX_RCPLL_CFG0(), |r| {
            r.set_pllf_oor_recal_ena(0);
        })?;

        hl::sleep_for(2);
        let stat0 = v.read(rx_rcpll.SD10G65_RX_RCPLL_STAT0())?;
        if stat0.pllf_lock_stat() != 1 {
            return Err(VscError::RxPllLockFailed);
        }
        let stat1 = v.read(rx_rcpll.SD10G65_RX_RCPLL_STAT1())?;
        if stat1.pllf_fsm_stat() != 13 {
            return Err(VscError::RxPllFsmFailed);
        }

        ////////////////////////////////////////////////////////////////////////
        // jaguar2c_apc10g_register_cfg, assuming
        //      ib_cal_only = false
        //      incl_ld_cal = false
        //      skip_cal = false
        //      is_2pt_cal = false
        //      force_eqz_l = false
        //      force_eqz_c = false
        // (these are the defaults from `vtss_sd10g65_setup_apc_args_init`)
        let dev_dig = XGDIG(index);
        let apc = dev_dig.SD10G65_APC();
        v.modify(apc.APC_COMMON_CFG0(), |r| {
            r.set_apc_fsm_recover_mode(1);
            r.set_skip_cal(0);
            r.set_reset_apc(1);
            r.set_apc_direct_ena(1);
            r.set_if_width(self.if_mode_sel);
        })?;
        v.modify(apc.APC_LD_CAL_CFG(), |r| {
            r.set_cal_clk_div(3);
        })?;
        v.modify(apc.APC_IS_CAL_CFG1(), |r| {
            r.set_par_data_num_ones_thres(self.if_width / 4);
            r.set_cal_num_iterations(1);
        })?;
        v.modify(apc.APC_EQZ_COMMON_CFG(), |r| {
            r.set_eqz_gain_auto_restart(0);
        })?;
        v.modify(apc.APC_PARCTRL_FSM1_TIMER_CFG(), |r| {
            r.set_fsm1_op_time(50000);
        })?;
        v.modify(apc.APC_PARCTRL_SYNC_CFG(), |r| {
            r.set_fsm1_op_mode(1); // one-time operation
        })?;
        v.modify(apc.APC_EQZ_LD_CTRL(), |r| {
            r.set_ld_lev_ini(self.apc_preset.ld_lev_ini.into());
        })?;
        v.modify(apc.APC_EQZ_LD_CTRL_CFG0(), |r| {
            r.set_ld_t_deadtime_wrk(65535);
            r.set_ld_t_timeout_wrk(1000);
        })?;
        v.modify(apc.APC_EQZ_LD_CTRL_CFG1(), |r| {
            r.set_ld_t_deadtime_cal(65535);
            r.set_ld_t_timeout_cal(1000);
        })?;
        v.modify(apc.APC_EQZ_PAT_MATCH_CFG0(), |r| {
            r.set_eqz_c_pat_mask(15);
            r.set_eqz_c_pat_match(5);
            r.set_eqz_l_pat_mask(15);
            r.set_eqz_l_pat_match(5);
        })?;
        v.modify(apc.APC_EQZ_PAT_MATCH_CFG1(), |r| {
            r.set_eqz_offs_pat_mask(7);
            r.set_eqz_offs_pat_match(2);
            r.set_eqz_agc_pat_mask(15);
            r.set_eqz_agc_pat_match(5);
        })?;
        v.modify(apc.APC_EQZ_OFFS_PAR_CFG(), |r| {
            r.set_eqz_offs_chg_mode(0);
            r.set_eqz_offs_range_sel(self.apc_preset.range_sel.into());
            r.set_eqz_offs_max(0xA0);
            r.set_eqz_offs_ini(0x80);
            r.set_eqz_offs_min(0x60);
        })?;
        v.modify(apc.APC_EQZ_OFFS_CTRL(), |r| {
            r.set_eqz_offs_sync_mode(1);
        })?;
        v.modify(apc.APC_EQZ_AGC_PAR_CFG(), |r| {
            r.set_eqz_agc_chg_mode(0);
            r.set_eqz_agc_range_sel(self.apc_preset.range_sel.into());
            r.set_eqz_agc_min(self.apc_preset.agc_min.into());
            r.set_eqz_agc_max(self.apc_preset.agc_max.into());
            r.set_eqz_agc_ini(self.apc_preset.agc_ini.into());
        })?;
        v.modify(apc.APC_EQZ_AGC_CTRL(), |r| {
            r.set_eqz_agc_sync_mode(1);
        })?;
        v.modify(apc.APC_EQZ_OFFS_PAR_CFG(), |r| {
            r.set_eqz_offs_dir_sel(0);
        })?;

        if self.high_data_rate {
            v.modify(apc.APC_EQZ_L_PAR_CFG(), |r| {
                r.set_eqz_l_chg_mode(0);
                r.set_eqz_l_range_sel(
                    (self.apc_preset.range_sel + self.apc_preset.l_rs_offs)
                        .into(),
                );
                r.set_eqz_l_max(self.apc_preset.l_max.into());
                r.set_eqz_l_min(self.apc_preset.l_min.into());
                r.set_eqz_l_ini(self.apc_preset.l_ini.into());
            })?;

            v.modify(apc.APC_EQZ_L_CTRL(), |r| {
                r.set_eqz_l_sync_mode(1);
            })?;

            v.modify(apc.APC_EQZ_C_PAR_CFG(), |r| {
                r.set_eqz_c_chg_mode(0);
                r.set_eqz_c_range_sel(
                    (self.apc_preset.range_sel + self.apc_preset.c_rs_offs)
                        .into(),
                );
                r.set_eqz_c_max(self.apc_preset.c_max.into());
                r.set_eqz_c_min(self.apc_preset.c_min.into());
                r.set_eqz_c_ini(self.apc_preset.c_ini.into());
            })?;
            v.modify(apc.APC_EQZ_C_CTRL(), |r| {
                r.set_eqz_c_sync_mode(1);
            })?;
        } else {
            // "low data rates -> force L and C to 0"
            v.modify(apc.APC_EQZ_L_PAR_CFG(), |r| {
                r.set_eqz_l_chg_mode(1);
                r.set_eqz_l_ini(0); // "lowest value"
            })?;
            v.modify(apc.APC_EQZ_L_CTRL(), |r| {
                r.set_eqz_l_sync_mode(0); // "disabled"
            })?;

            v.modify(apc.APC_EQZ_C_PAR_CFG(), |r| {
                r.set_eqz_c_chg_mode(1);
                r.set_eqz_c_ini(0); // "lowest value"
            })?;
            v.modify(apc.APC_EQZ_C_CTRL(), |r| {
                r.set_eqz_c_sync_mode(0); // "disabled"
            })?;
        }

        v.modify(apc.APC_DFE1_PAR_CFG(), |r| {
            r.set_dfe1_chg_mode(0);
            r.set_dfe1_range_sel(self.apc_preset.range_sel.into());
            r.set_dfe1_max(self.apc_preset.dfe1_max.into());
            r.set_dfe1_min(self.apc_preset.dfe1_min.into());
            r.set_dfe1_ini(64);
        })?;
        v.modify(apc.APC_DFE1_CTRL(), |r| {
            r.set_dfe1_sync_mode(1);
        })?;
        v.modify(apc.APC_DFE2_PAR_CFG(), |r| {
            r.set_dfe2_chg_mode(0);
            r.set_dfe2_range_sel(self.apc_preset.range_sel.into());
            r.set_dfe2_max(if self.optimize_for_1g { 36 } else { 48 });
            r.set_dfe2_ini(32);
            r.set_dfe2_min(0);
        })?;
        v.modify(apc.APC_DFE2_CTRL(), |r| {
            r.set_dfe2_sync_mode(1);
        })?;
        v.modify(apc.APC_DFE3_PAR_CFG(), |r| {
            r.set_dfe3_chg_mode(0);
            r.set_dfe3_range_sel(self.apc_preset.range_sel.into());
            r.set_dfe3_max(if self.optimize_for_1g { 20 } else { 31 });
            r.set_dfe3_ini(16);
            r.set_dfe3_min(0);
        })?;
        v.modify(apc.APC_DFE3_CTRL(), |r| {
            r.set_dfe3_sync_mode(1);
        })?;
        v.modify(apc.APC_DFE4_PAR_CFG(), |r| {
            r.set_dfe4_chg_mode(0);
            r.set_dfe4_range_sel(self.apc_preset.range_sel.into());
            r.set_dfe4_max(if self.optimize_for_1g { 20 } else { 31 });
            r.set_dfe4_ini(16);
            r.set_dfe4_min(0);
        })?;
        v.modify(apc.APC_DFE4_CTRL(), |r| {
            r.set_dfe4_sync_mode(1);
        })?;

        // Input calibration?
        v.modify(ib.SD10G65_IB_CFG6(), |r| {
            r.set_ib_sam_offs_adj(31);
        })?;

        // In the SDK, these three blocks are gated by "is_2pt_cal[0] == 0"
        v.modify(ib.SD10G65_IB_CFG8(), |r| {
            r.set_ib_inv_thr_cal_val(0);
        })?;
        v.modify(apc.APC_IS_CAL_CFG0(), |r| {
            r.set_cpmd_thres_init(31);
            r.set_vsc_thres_init(31);
            r.set_skip_threshold_cal(1);
        })?;
        v.modify(apc.APC_IS_CAL_CFG1(), |r| {
            r.set_cal_vsc_offset_tgt(1);
        })?;

        v.modify(ib.SD10G65_IB_CFG0(), |r| {
            r.set_ib_dfe_ena(0);
        })?;
        v.modify(apc.APC_COMMON_CFG0(), |r| {
            r.set_apc_mode(1); // "manual mode for manual ib_cal"
        })?;
        v.modify(apc.APC_COMMON_CFG0(), |r| {
            r.set_reset_apc(0); // Release reset
        })?;
        hl::sleep_for(1);
        v.modify(apc.APC_IS_CAL_CFG1(), |r| {
            r.set_start_offscal(1);
        })?;

        {
            // This is the calculation for `calibration_time_ms[1]` in the SDK
            let cal_clk_div = 3;
            let cal_num_iterations = 1;
            hl::sleep_for(
                ((1u64 << (2 * cal_clk_div))
                    * (cal_num_iterations + 1)
                    * 156500
                    * self.if_width as u64
                    + (self.f_pll_khz_plain as u64 - 1))
                    / (self.f_pll_khz_plain as u64),
            );
            // TODO: why is this needed?  It's not in the SDK, but the system
            // doesn't configure without this pause.
            hl::sleep_for(100);
        }
        let cfg1 = v.read(apc.APC_IS_CAL_CFG1())?;
        if cfg1.offscal_done() != 1 {
            return Err(VscError::OffsetCalFailed);
        }
        v.modify(apc.APC_IS_CAL_CFG1(), |r| {
            r.set_start_offscal(0);
        })?;

        v.modify(ib.SD10G65_IB_CFG0(), |r| {
            r.set_ib_dfe_ena(1);
        })?;

        v.modify(apc.APC_COMMON_CFG0(), |r| {
            r.set_skip_cal(1);
            r.set_apc_mode(2); // "Perform calibrarion and run FSM1"
        })?;

        Ok(())
    }
}

/// Equivalent to `vtss_sd10g65_preset_t`
#[derive(Copy, Clone, PartialEq)]
enum SerdesPresetType {
    DacHw, // VTSS_SD10G65_DAC_HW
    KrHw,  // VTSS_SD10G65_KR_HW, i.e. 10GBASE-KR
}

/// Equivalent to `vtss_sd10g65_preset_struct_t`
#[derive(Copy, Clone, Debug, PartialEq)]
struct SerdesRxPreset {
    synth_phase_data: u8,
    ib_main_thres_offs: u8,
    ib_vscope_hl_offs: u8,
    ib_bias_adj: u8,
    ib_sam_offs_adj: u8,
    ib_tc_dfe: u8,
    ib_tc_eq: u8,
    ib_ena_400_inp: u8,
    ib_eqz_l_mode: u8,
    ib_eqz_c_mode: u8,
    ib_dfe_gain_adj: u8,
    ib_rib_adj: u8,
    ib_eq_ld1_offset: u8,
    ib_eqz_c_adj: u8,
    synth_dv_ctrl_i1e: u8,
}

impl SerdesRxPreset {
    fn new(t: SerdesPresetType) -> Self {
        // Based on `vtss_sd10g65_set_default_preset_values` and code in
        // `vtss_calc_sd10g65_setup_rx`.
        match t {
            SerdesPresetType::DacHw => Self {
                synth_phase_data: 54,
                ib_main_thres_offs: 0,
                ib_vscope_hl_offs: 10,
                ib_bias_adj: 31,
                ib_sam_offs_adj: 16,
                ib_eq_ld1_offset: 20,
                ib_eqz_l_mode: 0,
                ib_eqz_c_mode: 0,
                ib_dfe_gain_adj: 63,
                ib_rib_adj: 8,
                ib_tc_eq: 0,
                ib_tc_dfe: 0,
                ib_ena_400_inp: 1,
                ib_eqz_c_adj: 0,
                synth_dv_ctrl_i1e: 0,
            },
            SerdesPresetType::KrHw => Self {
                synth_phase_data: 54,
                ib_main_thres_offs: 0,
                ib_vscope_hl_offs: 10,
                ib_bias_adj: 31,
                ib_sam_offs_adj: 16,
                ib_eq_ld1_offset: 20,
                ib_eqz_l_mode: 3,
                ib_eqz_c_mode: 1,
                ib_dfe_gain_adj: 63,
                ib_rib_adj: 8,
                ib_tc_eq: 0,
                ib_tc_dfe: 0,
                ib_ena_400_inp: 1,
                ib_eqz_c_adj: 0,
                synth_dv_ctrl_i1e: 0,
            },
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
struct SerdesApcPreset {
    ld_lev_ini: u8,
    range_sel: u8,
    dfe1_min: u8,
    dfe1_max: u8,
    c_min: u8,
    c_max: u8,
    c_ini: u8,
    c_rs_offs: u8,
    l_min: u8,
    l_max: u8,
    l_ini: u8,
    l_rs_offs: u8,
    agc_min: u8,
    agc_max: u8,
    agc_ini: u8,
}

/// Presets for Automatic Pararameter Control configuration
impl SerdesApcPreset {
    /// Based on `vtss_sd10g65_apc_set_default_preset_values` and
    /// `vtss_calc_sd10g65_setup_apc`
    fn new(t: SerdesPresetType, optimize_for_1g: bool) -> Self {
        match t {
            SerdesPresetType::DacHw => Self {
                ld_lev_ini: 4,
                range_sel: 20,
                dfe1_min: 0,
                dfe1_max: if optimize_for_1g { 68 } else { 127 },
                c_min: 4,
                c_max: 31,
                c_ini: 25,
                c_rs_offs: 3,
                l_min: 8,
                l_max: 62,
                l_ini: 50,
                l_rs_offs: 2,
                agc_min: 0,
                agc_max: 216,
                agc_ini: 168,
            },
            SerdesPresetType::KrHw => Self {
                ld_lev_ini: 8,
                range_sel: 20,
                dfe1_min: 0,
                dfe1_max: 127,
                c_min: 0,
                c_max: 31,
                c_ini: 11,
                c_rs_offs: 3,
                l_min: 0,
                l_max: 124,
                l_ini: 44,
                l_rs_offs: 1,
                agc_min: 0,
                agc_max: 248,
                agc_ini: 88,
            },
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct SynthSettingsCalc {
    freq_mult: u16,
    freqm: u64,
    freqn: u64,
}
impl SynthSettingsCalc {
    /// `sd10g65_synth_settings_calc`
    fn new(num_in: u64, div_in: u64) -> SynthSettingsCalc {
        let freq_mult = ((8192u64 * num_in) / div_in) as u16;
        let numerator = (8192u64 * num_in) - (freq_mult as u64 * div_in);

        let (freqm, freqn) = if numerator == 0 {
            (0, 1u64 << 35)
        } else {
            let gcd = calc_gcd(numerator, div_in);
            let mut freqm = numerator / gcd;
            let mut freqn = numerator / gcd;

            // "Choose largest possible values to keep adaption time low"
            while freqn < (1u64 << 35) {
                freqm <<= 1;
                freqn <<= 1;
            }
            (freqm, freqn)
        };
        SynthSettingsCalc {
            freq_mult,
            freqm,
            freqn,
        }
    }
}

/// `sd10g65_calc_gcd`
fn calc_gcd(num_in: u64, mut div: u64) -> u64 {
    let mut rem = num_in / div;
    while rem != 0 {
        let num = div;
        div = rem;
        rem = num / div;
    }
    div
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FrequencySetup {
    f_pll_khz: u32,
    ratio_num: u32,
    ratio_den: u32,
}
impl FrequencySetup {
    pub fn new(mode: Mode) -> Self {
        match mode {
            Mode::Lan10g => FrequencySetup {
                // 10.3125Gbps
                f_pll_khz: 10_000_000,
                ratio_num: 66,
                ratio_den: 64,
            },
            Mode::Sgmii => FrequencySetup {
                // ~1.25Gbps
                f_pll_khz: 1_000_000,
                ratio_num: 10,
                ratio_den: 8,
            },
        }
    }
}

/// Roughly based on `vtss_sd10g65_synth_mult_calc_rslt_t`
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct SynthMultCalc {
    speed_sel: bool, // SYNTH_SPEED_SEL
    fbdiv_sel: u8,   // SYNTH_FBDIV_SEL
    settings: SynthSettingsCalc,
    freq_mult_byp: FrequencyDecoderBypass,
    tx_ls_speed: u8, // Lane sync speed. Only used when setting up the synthesizer for a TX macro
    tx_cs_speed: u8, // Common sync speed. Only used when setting up the synthesizer for a TX macro
    rx_fb_step: u8, // selects step width for sync output. Only used when setting up the synthesizer for a RX macro
    rx_i2_step: u8, // selects step width for integrator2. Only used when setting up the synthesizer for a RX macro
}

impl SynthMultCalc {
    /// `sd10g65_synth_mult_calc`
    fn new(f_pll_in: &FrequencySetup) -> Result<SynthMultCalc, VscError> {
        let num_in_tmp =
            (f_pll_in.f_pll_khz as u64) * (f_pll_in.ratio_num as u64);
        let div_in_tmp = (f_pll_in.ratio_den as u64) * 2_500_000;
        let dr_khz = num_in_tmp / (f_pll_in.ratio_den as u64); // = f_pll_khz_plain?

        let mut out = SynthMultCalc::default();

        let div_in_tmp = match dr_khz {
            0..=1_666_666 => {
                return Err(VscError::SerdesFrequencyTooLow(dr_khz));
            }
            1_666_667..=3_333_333 => {
                out.rx_fb_step = 3;
                div_in_tmp
            }
            3_333_334..=6_666_666 => {
                out.fbdiv_sel = 1;
                out.tx_cs_speed = 1;
                out.rx_fb_step = 2;
                2 * div_in_tmp
            }
            6_666_667..=13_333_333 => {
                out.fbdiv_sel = 2;
                out.tx_ls_speed = 1;
                out.tx_cs_speed = 1;
                4 * div_in_tmp
            }
            _ => return Err(VscError::SerdesFrequencyTooHigh(dr_khz)),
        };
        out.settings = SynthSettingsCalc::new(num_in_tmp, div_in_tmp);

        out.speed_sel = dr_khz < 5_000_000;
        out.freq_mult_byp =
            FrequencyDecoderBypass::new(out.settings.freq_mult)?;

        Ok(out)
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct FrequencyDecoderBypass {
    freq_mult: u16,
    freq_mult_hi: u8,
}

impl FrequencyDecoderBypass {
    fn new(freq_mult: u16) -> Result<Self, VscError> {
        let freq_sign = freq_mult >> 13;
        let freq_abs = if freq_sign == 1 {
            freq_mult
        } else {
            !freq_mult
        } & 0xfff;

        // Comments in the SDK suggest this was reverse-engineered to work
        // around a Verilog bug, so I don't expect to understand what's
        // going on here.

        // This ends up wrapping in the original SDK, so we use wrapping_sub
        // here to avoid Panicking! At the Arithmetic Underflow
        let tri_2g5 = Self::tri_dec((freq_abs.wrapping_sub(684) >> 10) & 0x3)?;
        let tri_625m = Self::tri_dec((freq_abs.wrapping_sub(172) >> 8) & 0x3)?;
        let tri_156m = Self::tri_dec((freq_abs.wrapping_sub(44) >> 6) & 0x3)?;
        let bi_39m = Self::bi_dec((freq_abs.wrapping_sub(12) >> 5) & 0x1)?;
        let tri_20m = Self::lt_dec((freq_abs.wrapping_add(4) >> 3) & 0x3)?;
        let ls_5m = Self::ls_dec((freq_abs.wrapping_sub(0) >> 0) & 0x7)?;

        let ena_2g5_dec = (tri_2g5 >> 2) & 1;
        let dir_2g5_dec = ((tri_2g5 >> 1) ^ !freq_sign) & 1;
        let spd_2g5_dec = (tri_2g5 >> 0) & 1;

        let ena_625m_dec = (tri_625m >> 2) & 1;
        let dir_625m_dec = ((tri_625m >> 1) ^ !freq_sign) & 1;
        let spd_625m_dec = (tri_625m >> 0) & 1;

        let ena_156m_dec = (tri_156m >> 2) & 1;
        let dir_156m_dec = ((tri_156m >> 1) ^ !freq_sign) & 1;
        let spd_156m_dec = (tri_156m >> 0) & 1;

        let ena_39m_dec = (bi_39m >> 1) & 1;
        let dir_39m_dec = ((bi_39m >> 0) ^ !freq_sign) & 1;

        let ena_20m_dec = (tri_20m >> 2) & 1;
        let dir_20m_pre = ((tri_20m >> 1) ^ !freq_sign) & 1;
        let spd_20m_dec = (tri_20m >> 0) & 1;

        let dir_5m_pre = ((ls_5m >> 3) ^ !freq_sign) & 1;
        let ena_2m5_dec = (ls_5m >> 2) & 1;
        let ena_1m25_dec = (ls_5m >> 1) & 1;
        let inv_sd_dec = ((ls_5m >> 0) ^ !freq_sign) & 1;

        let dir_ls_dec = dir_5m_pre;
        let dir_20m_dec = (dir_20m_pre ^ !dir_5m_pre) & 1;

        let freq_mult_hi = ((ena_2g5_dec << 3)
            | (dir_2g5_dec << 2)
            | (spd_2g5_dec << 1)
            | (ena_625m_dec << 0)) as u8
            ^ 0x4;

        let freq_mult = ((dir_625m_dec << 13)
            | (spd_625m_dec << 12)
            | (ena_156m_dec << 11)
            | (dir_156m_dec << 10)
            | (spd_156m_dec << 9)
            | (ena_39m_dec << 8)
            | (dir_39m_dec << 7)
            | (dir_ls_dec << 6)
            | (ena_20m_dec << 5)
            | (dir_20m_dec << 4)
            | (spd_20m_dec << 3)
            | (ena_2m5_dec << 2)
            | (ena_1m25_dec << 1)
            | (inv_sd_dec << 0))
            ^ 0x24D0;

        Ok(Self {
            freq_mult,
            freq_mult_hi,
        })
    }

    /// sd10g65_tri_dec
    fn tri_dec(u: u16) -> Result<u16, VscError> {
        match u {
            0 => Ok(6),
            1 => Ok(7),
            2 => Ok(4),
            3 => Ok(0),
            i => Err(VscError::TriDecFailed(i)),
        }
    }
    /// sd10g65_bi_dec
    fn bi_dec(u: u16) -> Result<u16, VscError> {
        match u {
            0 => Ok(3),
            1 => Ok(1),
            i => Err(VscError::BiDecFailed(i)),
        }
    }
    /// sd10g65_lt_dec
    fn lt_dec(u: u16) -> Result<u16, VscError> {
        match u {
            0 => Ok(0),
            1 => Ok(6),
            2 => Ok(5),
            3 => Ok(4),
            i => Err(VscError::LtDecFailed(i)),
        }
    }
    /// sd10g65_ls_dec
    fn ls_dec(u: u16) -> Result<u16, VscError> {
        match u {
            0 => Ok(8),
            1 => Ok(10),
            2 => Ok(12),
            3 => Ok(14),
            4 => Ok(7),
            5 => Ok(5),
            6 => Ok(3),
            7 => Ok(1),
            i => Err(VscError::LsDecFailed(i)),
        }
    }
}
