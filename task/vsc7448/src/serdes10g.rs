/// Tools for working with the 10G SERDES (sd10g65 in the SDK)
use crate::{Vsc7448Spi, VscError};
use userlib::hl;
use vsc7448_pac::Vsc7448;

pub struct SerdesConfig {
    f_pll: FrequencySetup,

    mult: SynthMultCalc,
    preset: SerdesRxPreset,

    half_rate_mode: bool,
    tx_synth_off_comp_ena: u32,
    pll_lpf_cur: u32,
    pll_lpf_res: u32,
    pllf_ref_cnt_end: u32,

    ib_bias_adj: u32,
}

impl SerdesConfig {
    pub fn new() -> Result<Self, VscError> {
        // `vtss_calc_sd10g65_setup_tx`
        let mut f_pll = get_frequency_setup(SerdesMode::Lan10g);
        let mut f_pll_khz_plain =
            ((f_pll.f_pll_khz as u64 * f_pll.ratio_num as u64)
                / (f_pll.ratio_den as u64)) as u32;

        let half_rate_mode = if f_pll_khz_plain < 2_500_000 {
            f_pll_khz_plain *= 2;
            f_pll.f_pll_khz *= 2;
            true
        } else {
            false
        };

        let mult = SynthMultCalc::new(&f_pll)?;

        let tx_synth_off_comp_ena =
            if f_pll_khz_plain > 10_312_500 { 31 } else { 23 };

        let (pll_lpf_cur, pll_lpf_res) = if f_pll_khz_plain > 7000000 {
            (3, 10)
        } else if f_pll_khz_plain > 3000000 {
            (2, 15)
        } else {
            (0, 10)
        };

        let if_width = 32;
        let pllf_ref_cnt_end = if half_rate_mode {
            (if_width * 64 * 1000000) / (f_pll_khz_plain >> 1)
        } else {
            (if_width * 64 * 1000000) / f_pll_khz_plain
        };

        ////////////////////////////////////////////////////////////////////////
        // `vtss_calc_sd10g65_setup_rx
        let ib_bias_adj = 31; // This can change depending on cable type!
        let preset = SerdesRxPreset::new(SerdesPresetType::DacHw);

        Ok(Self {
            f_pll,
            mult,
            preset,
            half_rate_mode,

            tx_synth_off_comp_ena,
            pll_lpf_cur,
            pll_lpf_res,
            pllf_ref_cnt_end,

            ib_bias_adj,
        })
    }
    /// Based on `jaguar2c_sd10g_*_register_cfg`.  Any variables which aren't
    /// changed are converted into direct register assignments (rather than
    /// passing them around in the config struct).
    pub fn apply(
        &self,
        index: u32,
        v: &mut Vsc7448Spi,
    ) -> Result<(), VscError> {
        let dev = Vsc7448::XGANA(index);

        ////////////////////////////////////////////////////////////////////////
        //  `jaguar2c_sd10g_tx_register_cfg`
        let tx_synth = dev.SD10G65_TX_SYNTH();
        let ob = dev.SD10G65_OB();
        v.modify(ob.SD10G65_SBUS_TX_CFG(), |r| {
            r.set_sbus_bias_en(1);
        })?;
        v.modify(dev.SD10G65_OB().SD10G65_OB_CFG0(), |r| {
            r.set_en_ob(1);
        })?;
        v.modify(dev.SD10G65_TX_RCPLL().SD10G65_TX_RCPLL_CFG2(), |r| {
            r.set_pll_ena(1)
        })?;
        v.modify(tx_synth.SD10G65_TX_SYNTH_CFG0(), |r| {
            r.set_synth_ena(1);
            r.set_synth_spare_pool(7);
            r.set_synth_off_comp_ena(self.tx_synth_off_comp_ena);
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
            r.set_synth_hrate_ena(self.half_rate_mode.into());
            // These aren't in the datasheet, but are copied from the SDK
            r.set_synth_ena_sync_unit(1);
            r.set_synth_conv_ena(1);
            r.set_synth_ds_dir(0);
            r.set_synth_ds_speed(0);
            r.set_synth_ls_dir(0);
            r.set_synth_ls_ena(0); // TODO: CHECK THIS
        })?;
        v.modify(tx_synth.SD10G65_SSC_CFG1(), |r| {
            r.set_sync_ctrl_fsel(35);
        })?;
        // TODO: check ob.SD10G65_OB_CFG0/2 on a running device to make sure they're defaults

        let tx_rcpll = dev.SD10G65_TX_RCPLL();
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
        let ib = dev.SD10G65_IB();
        v.modify(ib.SD10G65_SBUS_RX_CFG(), |r| {
            r.set_sbus_spare_pool(0);
            r.set_sbus_bias_en(1);
        })?;

        let rx_rcpll = dev.SD10G65_RX_RCPLL();
        v.modify(rx_rcpll.SD10G65_RX_RCPLL_CFG2(), |r| {
            r.set_pll_ena(1);
        })?;

        let rx_synth = dev.SD10G65_RX_SYNTH();
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG0(), |r| {
            r.set_synth_ena(1);
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

            r.set_ib_bias_adj(self.ib_bias_adj);
        })?;
        // TODO: can we consolidate these write operations?
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
            // TODO: check FB_STEP and I2_STEP values against running system
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
            r.set_synth_phase_data(self.preset.synth_phase_data.into());
            r.set_synth_cpmd_dig_ena(0); // Not in MODE_FX100
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG0(), |r| {
            r.set_synth_p_step(1);
            r.set_synth_i1_step(1);
        })?;
        v.modify(rx_synth.SD10G65_RX_SYNTH_CFG2(), |r| {
            // This intentionally assigns the same value to both I1E and I1M,
            // based on the preset configuration.
            r.set_synth_dv_ctrl_i1e(self.preset.synth_dv_ctrl_i1e.into());
            r.set_synth_dv_ctrl_i1m(self.preset.synth_dv_ctrl_i1e.into());
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
            r.set_ib_rib_adj(self.preset.ib_rib_adj.into());
            r.set_ib_eqz_ena(1);
            r.set_ib_dfe_ena(1);
            r.set_ib_ld_ena(1);
            r.set_ib_ia_ena(1);
            r.set_ib_ia_sdet_ena(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG3(), |r| {
            r.set_ib_eq_ld1_offset(self.preset.ib_eq_ld1_offset.into());
            r.set_ib_ldsd_divsel(0);
            r.set_ib_ia_sdet_level(2);
            r.set_ib_sdet_sel(0);
        })?;
        v.modify(ib.SD10G65_IB_CFG5(), |r| {
            r.set_ib_offs_value(31);
            r.set_ib_calmux_ena(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG6(), |r| {
            r.set_ib_sam_offs_adj(self.preset.ib_sam_offs_adj.into());

            // Depends on chip family; our chip is a JAGUAR2C
            r.set_ib_auto_agc_adj(1);
        })?;
        v.modify(ib.SD10G65_IB_CFG7(), |r| {
            r.set_ib_dfe_gain_adj_s(1);
            r.set_ib_dfe_gain_adj(self.preset.ib_dfe_gain_adj.into());
            r.set_ib_dfe_offset_h(
                (4 + 19 * self.preset.ib_vscope_hl_offs as u32) / 8,
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
            r.set_ib_eqz_c_adj_ib(self.preset.ib_eqz_c_adj.into());
            r.set_ib_eqz_c_adj_es0(self.preset.ib_eqz_c_adj.into());
            r.set_ib_eqz_c_adj_es1(self.preset.ib_eqz_c_adj.into());
            r.set_ib_eqz_c_adj_es2(self.preset.ib_eqz_c_adj.into());
            r.set_ib_eqz_c_mode(self.preset.ib_eqz_c_mode.into());
            r.set_ib_eqz_l_mode(self.preset.ib_eqz_l_mode.into());
            r.set_ib_vscope_h_thres(
                (32 + self.preset.ib_vscope_hl_offs).into(),
            );
            r.set_ib_vscope_l_thres(
                (31 - self.preset.ib_vscope_hl_offs).into(),
            );
            r.set_ib_main_thres((32 + self.preset.ib_vscope_hl_offs).into());
        })?;
        v.modify(ib.SD10G65_IB_CFG11(), |r| {
            r.set_ib_ena_400_inp(self.preset.ib_ena_400_inp.into());
            r.set_ib_tc_dfe(self.preset.ib_tc_dfe.into());
            r.set_ib_tc_eq(self.preset.ib_tc_eq.into());
        })?;

        let des = dev.SD10G65_DES();
        // Leave CFG0 untouched from defaults (TODO: check)

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
        // jaguar2c_apc10g_register_cfg
        let dev_dig = Vsc7448::XGDIG(index);
        let apc = dev_dig.SD10G65_APC();
        v.modify(apc.APC_COMMON_CFG0(), |r| {
            r.set_apc_fsm_recover_mode(1);
            r.set_skip_cal(0);
            r.set_reset_apc(1);
            r.set_apc_direct_ena(1);
        })?;
        v.modify(apc.APC_LD_CAL_CFG(), |r| {
            r.set_cal_clk_div(3);
        })?;

        Ok(())
    }
}

/// Equivalent to `vtss_sd10g65_preset_t`
enum SerdesPresetType {
    DacHw,
}

/// Equivalent to `vtss_sd10g65_preset_struct_t`
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
    pll_vreg18: u8,
    pll_vco_cur: u8,
    ib_sig_sel: u8,
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
                pll_vreg18: 10,
                pll_vco_cur: 7,
                ib_sig_sel: 0,
                ib_eqz_c_adj: 0,
                synth_dv_ctrl_i1e: 0,
            },
        }
    }
}

struct SerdesApcPreset {
    ld_lev_ini: u8,
    range_sel: u8,
    dfe1_min: u8,
    dfe1_max: u8,
    gain_ini: u16,
    gain_adj_ini: u8,
    gain_chg_mode: u8,
    c_min: u8,
    c_max: u8,
    c_ini: u8,
    c_rs_offs: u8,
    c_chg_mode: u8,
    l_min: u8,
    l_max: u8,
    l_ini: u8,
    l_rs_offs: u8,
    l_chg_mode: u8,
    agc_min: u8,
    agc_max: u8,
    agc_ini: u8,
    lc_smartctrl: u8,
}

/// Presets for Automatic Pararameter Control configuration
impl SerdesApcPreset {
    /// Based on `vtss_sd10g65_apc_set_default_preset_values` and
    /// `vtss_calc_sd10g65_setup_apc`
    fn new(t: SerdesPresetType) -> Self {
        match t {
            SerdesPresetType::DacHw => Self {
                ld_lev_ini: 4,
                range_sel: 20,
                dfe1_min: 0,
                dfe1_max: 127,
                gain_ini: 0,
                gain_adj_ini: 0,
                gain_chg_mode: 0,
                c_min: 4,
                c_max: 31,
                c_ini: 25,
                c_rs_offs: 3,
                c_chg_mode: 0,
                l_min: 8,
                l_max: 62,
                l_ini: 50,
                l_rs_offs: 2,
                l_chg_mode: 0,
                agc_min: 0,
                agc_max: 216,
                agc_ini: 168,
                lc_smartctrl: 0,
            },
        }
    }
}

#[derive(Default)]
struct SynthSettingsCalc {
    freq_mult: u16,
    freqm: u64,
    freqn: u64,
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

/// `sd10g65_synth_settings_calc`
fn synth_settings_calc(num_in: u64, div_in: u64) -> SynthSettingsCalc {
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

enum SerdesMode {
    Lan10g,
}

pub struct FrequencySetup {
    f_pll_khz: u32,
    ratio_num: u32,
    ratio_den: u32,
}
fn get_frequency_setup(mode: SerdesMode) -> FrequencySetup {
    match mode {
        SerdesMode::Lan10g => FrequencySetup {
            f_pll_khz: 10_000_000,
            ratio_num: 66, // 10.3125Gbps
            ratio_den: 64,
        },
        // Other modes aren't supported!
    }
}

/// Roughly based on `vtss_sd10g65_synth_mult_calc_rslt_t`
#[derive(Default)]
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
    /// sd10g65_synth_mult_calc
    fn new(f_pll_in: &FrequencySetup) -> Result<SynthMultCalc, VscError> {
        let num_in_tmp =
            (f_pll_in.f_pll_khz as u64) / (f_pll_in.ratio_num as u64);
        let div_in_tmp = (f_pll_in.ratio_den as u64) * 2_500_000;
        let dr_khz = num_in_tmp / (f_pll_in.ratio_den as u64); // = f_pll_khz_plain?

        let mut out = SynthMultCalc::default();

        out.settings = match dr_khz {
            0..=1_666_666 => {
                return Err(VscError::SerdesFrequencyTooLow(dr_khz))
            }
            1_666_667..=3_333_333 => {
                out.rx_fb_step = 3;
                synth_settings_calc(num_in_tmp, div_in_tmp)
            }
            3_333_334..=6_666_666 => {
                out.fbdiv_sel = 1;
                out.tx_cs_speed = 1;
                out.rx_fb_step = 2;
                synth_settings_calc(num_in_tmp, 2 * div_in_tmp)
            }
            6_666_667..=13_333_333 => {
                out.fbdiv_sel = 2;
                out.tx_ls_speed = 1;
                out.tx_cs_speed = 1;
                synth_settings_calc(num_in_tmp, 4 * div_in_tmp)
            }
            _ => return Err(VscError::SerdesFrequencyTooHigh(dr_khz)),
        };
        out.speed_sel = if dr_khz < 5_000_000 { true } else { false };
        out.freq_mult_byp =
            FrequencyDecoderBypass::new(out.settings.freq_mult)?;

        Ok(out)
    }
}

#[derive(Default)]
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

        let tri_2g5 = Self::tri_dec(((freq_abs - 684) >> 10) & 0x3)?;
        let tri_625m = Self::tri_dec(((freq_abs - 172) >> 8) & 0x3)?;
        let tri_156m = Self::tri_dec(((freq_abs - 44) >> 6) & 0x3)?;
        let bi_39m = Self::bi_dec(((freq_abs - 12) >> 5) & 0x1)?;
        let tri_20m = Self::lt_dec(((freq_abs + 4) >> 3) & 0x3)?;
        let ls_5m = Self::ls_dec(((freq_abs - 0) >> 0) & 0x7)?;

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
