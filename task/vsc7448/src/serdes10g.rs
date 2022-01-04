/// Tools for working with the 10G SERDES (sd10g65 in the SDK)
use crate::{Vsc7448Spi, VscError};
use userlib::hl;
use vsc7448_pac::Vsc7448;

pub struct SetupTx {
    f_pll: FrequencySetup,

    mult: SynthMultCalc,

    tx_synth_hrate_ena: bool,
    tx_synth_off_comp_ena: u32,
    pll_lpf_cur: u32,
    pll_lpf_res: u32,
    pllf_ref_cnt_end: u32,
}

impl SetupTx {
    /// `vtss_calc_sd10g65_setup_tx`
    pub fn new() -> Result<Self, VscError> {
        let mut f_pll = get_frequency_setup(SerdesMode::Lan10g);
        let mut f_pll_khz_plain =
            ((f_pll.f_pll_khz as u64 * f_pll.ratio_num as u64)
                / (f_pll.ratio_den as u64)) as u32;

        let tx_synth_hrate_ena = if f_pll_khz_plain < 2_500_000 {
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
        let pllf_ref_cnt_end = if tx_synth_hrate_ena {
            (if_width * 64 * 1000000) / (f_pll_khz_plain >> 1)
        } else {
            (if_width * 64 * 1000000) / f_pll_khz_plain
        };

        // ... AND MORE
        Ok(Self {
            f_pll,
            mult,
            tx_synth_hrate_ena,
            tx_synth_off_comp_ena,
            pll_lpf_cur,
            pll_lpf_res,
            pllf_ref_cnt_end,
        })
    }
    /// Based on `jaguar2c_sd10g_tx_register_cfg`, but turning variables
    /// which never change into direct register assignments (rather than
    /// assigning them and passing them around in the config struct)
    pub fn apply(
        &self,
        index: u32,
        v: &mut Vsc7448Spi,
    ) -> Result<(), VscError> {
        let dev = Vsc7448::XGANA(index);
        let tx_synth = dev.SD10G65_TX_SYNTH();
        let ob = dev.SD10G65_OB();
        v.modify(ob.SD10G65_SBUS_TX_CFG(), |r| r.set_sbus_bias_en(1))?;
        v.modify(dev.SD10G65_IB().SD10G65_SBUS_RX_CFG(), |r| {
            r.set_sbus_bias_en(1)
        })?;
        v.modify(dev.SD10G65_OB().SD10G65_OB_CFG0(), |r| r.set_en_ob(1))?;
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
            r.set_synth_hrate_ena(self.tx_synth_hrate_ena.into());
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
        // TODO: check ob.SD10G65_OB_CFG0/2 to make sure they're defaults

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
            return Err(VscError::PllLockFailed);
        }
        let stat1 = v.read(tx_rcpll.SD10G65_TX_RCPLL_STAT1())?;
        if stat1.pllf_fsm_stat() != 13 {
            return Err(VscError::PllFsmFailed);
        }

        Ok(())
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
