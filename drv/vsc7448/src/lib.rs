// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

pub mod config;
pub mod mac;
pub mod miim_phy;
pub mod spi;

mod dev;
mod port;
mod serdes10g;
mod serdes1g;
mod serdes6g;

use crate::config::{PortConfig, PortDev, PortMap, PortMode, PortSerdes};
use userlib::hl::sleep_for;
use vsc7448_pac::{types::RegisterAddress, *};

pub use config::Speed;
pub use dev::DevGeneric;
pub use vsc_err::VscError;

use crate::dev::Dev10g;

/// This trait abstracts over various ways of talking to a VSC7448.
pub trait Vsc7448Rw {
    /// Writes to a VSC7448 register.  Depending on the underlying transit
    /// mechanism, this may panic if registers are written outside of the
    /// switch core block (0x71000000 to 0x72000000)
    fn write<T>(
        &self,
        reg: RegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u32: From<T>;

    fn read<T>(&self, reg: RegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u32>;

    /// Performs a write operation on the given register, where the value is
    /// calculated by calling f(0).  This is helpful as a way to reduce manual
    /// type information.
    ///
    /// The register must be in the switch core register block, i.e. having an
    /// address in the range 0x71000000-0x72000000; otherwise, this will panic.
    fn write_with<T, F>(
        &self,
        reg: RegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u32>,
        u32: From<T>,
        F: Fn(&mut T),
    {
        let mut data = 0.into();
        f(&mut data);
        self.write(reg, data)
    }

    /// Performs a read-modify-write operation on a VSC7448 register
    ///
    /// The register must be in the switch core register block, i.e. having an
    /// address in the range 0x71000000-0x72000000; otherwise, this will panic.
    fn modify<T, F>(
        &self,
        reg: RegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u32>,
        u32: From<T>,
        F: Fn(&mut T),
    {
        let mut data = self.read(reg)?;
        f(&mut data);
        self.write(reg, data)
    }

    /// Writes to a port mask, which is assumed to be a pair of adjacent
    /// registers representing all 53 ports (e.g. VLAN_PORT_MASK and
    /// VLAN_PORT_MASK1).
    fn write_port_mask<T>(
        &self,
        mut reg: RegisterAddress<T>,
        value: u64,
    ) -> Result<(), VscError>
    where
        T: From<u32>,
        u32: From<T>,
    {
        self.write(reg, (value as u32).into())?;
        reg.addr += 4; // Good luck!
        self.write(reg, (((value >> 32) as u32) & 0x1FFFFF).into())
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Top-level state wrapper for a VSC7448 chip.
pub struct Vsc7448<'a, R> {
    pub rw: &'a mut R,
}

impl<R: Vsc7448Rw> Vsc7448Rw for Vsc7448<'_, R> {
    /// Write a register to the VSC7448
    fn write<T>(
        &self,
        reg: RegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u32: From<T>,
    {
        self.rw.write(reg, value)
    }

    /// Read a register from the VSC7448
    fn read<T>(&self, reg: RegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u32>,
    {
        self.rw.read(reg)
    }
}

impl<'a, R: Vsc7448Rw> Vsc7448<'a, R> {
    pub fn new(rw: &'a mut R) -> Self {
        Self { rw }
    }

    /// Configures all ports in the system from a single `PortMap`
    pub fn configure_ports_from_map(
        &self,
        map: &PortMap,
    ) -> Result<(), VscError> {
        for p in 0..map.len() {
            if let Some(cfg) = map.port_config(p as u8) {
                self.configure_port_from_config(p as u8, cfg)?;
            }
        }
        self.apply_calendar()?;
        Ok(())
    }

    /// Configures a single port, given its number and the `PortConfig`
    fn configure_port_from_config(
        &self,
        p: u8,
        cfg: PortConfig,
    ) -> Result<(), VscError> {
        match cfg.mode {
            PortMode::Sgmii(_) => match cfg.serdes.0 {
                PortSerdes::Serdes10g => self.init_10g_sgmii(p, cfg),
                PortSerdes::Serdes1g | PortSerdes::Serdes6g => {
                    self.init_sgmii(p, cfg)
                }
            },
            PortMode::Qsgmii(_) => {
                if p % 4 == 0 {
                    self.init_qsgmii(p, cfg)
                } else {
                    // All QSGMII ports are initialized with the base port
                    // of the set, so we ignore other ports here.
                    Ok(())
                }
            }
            PortMode::Sfi => self.init_sfi(p, cfg),
        }
    }

    /// Initializes the given ports as an SFI connection.  The given ports must
    /// be in the range 49..=52, otherwise this function will panic.
    ///
    /// This will configure the appropriate DEV10G and SERDES10G.
    fn init_sfi(&self, p: u8, cfg: PortConfig) -> Result<(), VscError> {
        assert_eq!(cfg.mode, PortMode::Sfi);
        assert_eq!(cfg.dev.0, PortDev::Dev10g);

        let dev = Dev10g::new(cfg.dev.1)?;
        assert_eq!(dev.port(), p);

        dev.init_sfi(self.rw)?;
        // Disable ASM / DSM stat collection for this port, since that
        // data will be collected in the DEV10G instead
        self.modify(ASM().CFG().PORT_CFG(p), |r| {
            r.set_csc_stat_dis(1);
        })?;
        self.modify(DSM().CFG().BUF_CFG(p), |r| {
            r.set_csc_stat_dis(1);
        })?;

        assert_eq!(cfg.serdes.0, PortSerdes::Serdes10g);
        let serdes_cfg = serdes10g::Config::new(serdes10g::Mode::Lan10g)?;
        serdes_cfg.apply(cfg.serdes.1, self.rw)?;

        self.set_calendar_bandwidth(p, Bandwidth::Bw10G)?;
        Ok(())
    }

    /// Enables 100M SGMII for the given port, using Table 5 in the datasheet to
    /// convert from ports to DEV and SERDES.
    ///
    /// Each value in `ports` must be between 0 and 31, or 48 (the NPI port)
    fn init_sgmii(&self, p: u8, cfg: PortConfig) -> Result<(), VscError> {
        assert!(matches!(cfg.mode, PortMode::Sgmii(_)));

        let dev = match cfg.dev.0 {
            PortDev::Dev1g => DevGeneric::new_1g,
            PortDev::Dev2g5 => DevGeneric::new_2g5,
            _ => panic!("Invalid dev for SGMII"),
        }(cfg.dev.1)?;
        assert_eq!(dev.port(), p);

        dev.init_sgmii(self.rw, cfg.mode.speed())?;

        match cfg.serdes.0 {
            PortSerdes::Serdes1g => {
                serdes1g::Config::new(serdes1g::Mode::Sgmii)
                    .apply(cfg.serdes.1, self.rw)?
            }
            PortSerdes::Serdes6g => {
                serdes6g::Config::new(serdes6g::Mode::Sgmii)
                    .apply(cfg.serdes.1, self.rw)?
            }
            _ => panic!("Invalid SERDES in init_sgmii"),
        }

        self.set_calendar_bandwidth(p, Bandwidth::Bw1G)?;
        Ok(())
    }

    /// Enables QSGMII mode for blocks of four ports beginning at `start_port`.
    /// This will configure the appropriate DEV1G or DEV2G5 devices, and the
    /// appropriate SERDES6G, based on Table 8 in the datasheet;
    ///
    /// Each value in `start_ports` must be divisible by 4 and below 48;
    /// otherwise, this function will panic.
    fn init_qsgmii(&self, p: u8, cfg: PortConfig) -> Result<(), VscError> {
        assert!(matches!(cfg.mode, PortMode::Qsgmii(_)));
        assert_eq!(p % 4, 0);

        // Set a bit to enable QSGMII for these block
        self.modify(HSIO().HW_CFGSTAT().HW_CFG(), |r| {
            let mut e = r.qsgmii_ena();
            e |= 1 << (p / 4);
            r.set_qsgmii_ena(e);
        })?;
        assert!(p < 48);
        assert_eq!(p % 4, 0);

        let dev_type = match cfg.dev.0 {
            PortDev::Dev1g => DevGeneric::new_1g,
            PortDev::Dev2g5 => DevGeneric::new_2g5,
            _ => panic!("Invalid dev for QSGMII"),
        };

        // Reset the PCS TX clock domain.  In the SDK, this is accompanied
        // by the cryptic comment "BZ23738", which may refer to an errata
        // of some kind?
        for dev in (cfg.dev.1 + 1)..(cfg.dev.1 + 4) {
            self.modify(
                dev_type(dev)?.regs().DEV_CFG_STATUS().DEV_RST_CTRL(),
                |r| r.set_pcs_tx_rst(0),
            )?;
        }

        assert_eq!(cfg.serdes.0, PortSerdes::Serdes6g);
        let qsgmii_cfg = serdes6g::Config::new(serdes6g::Mode::Qsgmii);
        qsgmii_cfg.apply(cfg.serdes.1, self.rw)?;

        for dev in cfg.dev.1..(cfg.dev.1 + 4) {
            let dev = dev_type(dev)?;
            dev.init_sgmii(self.rw, cfg.mode.speed())?;
            self.modify(
                dev.regs().PCS1G_CFG_STATUS().PCS1G_ANEG_CFG(),
                |r| r.set_aneg_ena(1),
            )?;
        }
        for port in p..p + 4 {
            // Min bandwidth is 1G, so we'll use it here
            // (for both 100M and 1G port speeds)
            self.set_calendar_bandwidth(port, Bandwidth::Bw1G)?;
        }
        Ok(())
    }

    /// Configures a port to run DEV2G5_XX through a 10G SERDES.
    ///
    /// This is only valid for ports 49-52, and will panic otherwise; see
    /// Table 9 for details.
    fn init_10g_sgmii(&self, p: u8, cfg: PortConfig) -> Result<(), VscError> {
        assert!(p >= 49);
        assert!(p <= 52);

        assert!(matches!(cfg.mode, PortMode::Sgmii(_)));
        assert_eq!(cfg.serdes.0, PortSerdes::Serdes10g);

        let serdes10g_cfg_sgmii =
            serdes10g::Config::new(serdes10g::Mode::Sgmii)?;

        assert_eq!(cfg.dev.0, PortDev::Dev2g5);
        let d2g5 = DevGeneric::new_2g5(cfg.dev.1).unwrap();
        let d10g = Dev10g::new(p - 49).unwrap();
        assert_eq!(d2g5.port(), d10g.port());
        assert_eq!(d2g5.port(), p);

        // We have to disable and flush the 10G port that shadows this port
        port::port10g_flush(&d10g, self)?;

        // "Configure the 10G Mux mode to DEV2G5"
        self.modify(HSIO().HW_CFGSTAT().HW_CFG(), |r| match d10g.index() {
            0 => r.set_dev10g_0_mode(3),
            1 => r.set_dev10g_1_mode(3),
            2 => r.set_dev10g_2_mode(3),
            3 => r.set_dev10g_3_mode(3),
            d => panic!("Invalid DEV10G {}", d),
        })?;
        // This bit must be set when a 10G port runs below 10G speed
        self.modify(DSM().CFG().DEV_TX_STOP_WM_CFG(d2g5.port()), |r| {
            r.set_dev10g_shadow_ena(1);
        })?;
        serdes10g_cfg_sgmii.apply(cfg.serdes.1, self.rw)?;
        d2g5.init_sgmii(self.rw, cfg.mode.speed())?;

        self.set_calendar_bandwidth(p, Bandwidth::Bw1G)?;
        Ok(())
    }

    /// Performs initial configuration (endianness, soft reset, read padding) of
    /// the VSC7448, checks that its chip ID is correct, and brings core systems
    /// out of reset.
    ///
    /// Takes the REFCLK frequency, as well as an optional frequency for
    /// REFCLK2 (used to configure the PLL boost).
    pub fn init(
        &self,
        f1: RefClockFreq,
        f2: Option<RefClockFreq>,
    ) -> Result<(), VscError> {
        // Write the byte ordering / endianness configuration
        self.write(DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(), 0x81818181.into())?;

        // Trigger a soft reset
        self.write_with(DEVCPU_GCB().CHIP_REGS().SOFT_RST(), |r| {
            r.set_soft_chip_rst(1);
        })?;

        // Re-write byte ordering / endianness
        self.write(DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(), 0x81818181.into())?;

        // Configure reads to include padding bytes, since we're reading quickly
        self.write_with(DEVCPU_ORG().DEVCPU_ORG().IF_CFGSTAT(), |r| {
            r.set_if_cfg(spi::SPI_NUM_PAD_BYTES as u32);
        })?;

        let chip_id = self.read(DEVCPU_GCB().CHIP_REGS().CHIP_ID())?;
        if chip_id.rev_id() != 0x3
            || chip_id.part_id() != 0x7468
            || chip_id.mfg_id() != 0x74
            || chip_id.one() != 0x1
        {
            return Err(VscError::BadChipId(chip_id.into()));
        }

        // Core chip bringup, bringing all of the main subsystems out of reset
        // (based on `jr2_init_conf_set` in the SDK)
        self.modify(ANA_AC().STAT_GLOBAL_CFG_PORT().STAT_RESET(), |r| {
            r.set_reset(1)
        })?;
        self.modify(ASM().CFG().STAT_CFG(), |r| r.set_stat_cnt_clr_shot(1))?;
        self.modify(QSYS().RAM_CTRL().RAM_INIT(), |r| {
            r.set_ram_init(1);
            r.set_ram_ena(1);
        })?;
        self.modify(REW().RAM_CTRL().RAM_INIT(), |r| {
            r.set_ram_init(1);
            r.set_ram_ena(1);
        })?;
        // The VOP isn't in the datasheet, but it's in the SDK
        self.modify(VOP().RAM_CTRL().RAM_INIT(), |r| {
            r.set_ram_init(1);
            r.set_ram_ena(1);
        })?;
        self.modify(ANA_AC().RAM_CTRL().RAM_INIT(), |r| {
            r.set_ram_init(1);
            r.set_ram_ena(1);
        })?;
        self.modify(ASM().RAM_CTRL().RAM_INIT(), |r| {
            r.set_ram_init(1);
            r.set_ram_ena(1);
        })?;
        self.modify(DSM().RAM_CTRL().RAM_INIT(), |r| {
            r.set_ram_init(1);
            r.set_ram_ena(1);
        })?;

        // The RAM initialization should take about 40 µs, according to
        // the datasheet.
        sleep_for(1);

        // Confirm that the RAM_INIT bits have cleared themselves.
        // This should never fail, and there's not much we can do about it
        // if it _does_ fail.
        if self.read(QSYS().RAM_CTRL().RAM_INIT())?.ram_init() != 0
            || self.read(REW().RAM_CTRL().RAM_INIT())?.ram_init() != 0
            || self.read(VOP().RAM_CTRL().RAM_INIT())?.ram_init() != 0
            || self.read(ANA_AC().RAM_CTRL().RAM_INIT())?.ram_init() != 0
            || self.read(ASM().RAM_CTRL().RAM_INIT())?.ram_init() != 0
            || self.read(DSM().RAM_CTRL().RAM_INIT())?.ram_init() != 0
        {
            return Err(VscError::RamInitFailed);
        }

        // Enable the 5G PLL boost on the main clock, and optionally on
        // the secondary clock (if present)
        self.pll5g_setup(0, f1)?;
        if let Some(f2) = f2 {
            self.pll5g_setup(1, f2)?;
        }

        // Enable the queue system
        self.write_with(QSYS().SYSTEM().RESET_CFG(), |r| r.set_core_ena(1))?;

        self.high_speed_mode()?;

        sleep_for(105); // Minimum time between reset and SMI access

        Ok(())
    }

    /// Based on `vtss_lc_pll5g_setup` and various functions that it calls
    fn pll5g_setup(&self, i: u8, freq: RefClockFreq) -> Result<(), VscError> {
        let pll5g = HSIO().PLL5G_CFG(i);
        self.modify(pll5g.PLL5G_CFG4(), |r| {
            r.set_ib_ctrl(0x7600);
        })?;
        let loop_bw_res = match freq {
            RefClockFreq::Clk25MHz => 10,
            RefClockFreq::Clk125MHz => 14,
            RefClockFreq::Clk156p25MHz => 17,
        };
        self.modify(pll5g.PLL5G_CFG0(), |r| {
            r.set_ena_vco_contrh(0);
            r.set_loop_bw_res(loop_bw_res);
            r.set_selbgv820(4);
        })?;
        for _ in 0..=9 {
            self.modify(pll5g.PLL5G_CFG2(), |r| {
                r.set_disable_fsm(1);
            })?;
            self.modify(pll5g.PLL5G_CFG2(), |r| {
                r.set_disable_fsm(0);
            })?;
            sleep_for(10);
            let v = self
                .read(HSIO().PLL5G_STATUS(i).PLL5G_STATUS1())?
                .gain_stat();
            if v > 2 && v < 0xa {
                sleep_for(5);
                return Ok(());
            }
        }
        Err(VscError::LcPllInitFailed(i))
    }

    /// Based on the section of `jr2_init_conf_set` beginning with the comment
    /// "Configuring core clock to run 278MHz"
    ///
    /// In the SDK, this only runs for VTSS_TARGET_SPARX_IV_90, but in
    /// conversations with Microchip support, they say to use this on the
    /// VSC7448 as well (which is nominally a SPARX_IV_80 target)
    fn high_speed_mode(&self) -> Result<(), VscError> {
        for i in 0..2 {
            self.modify(HSIO().PLL5G_CFG(i).PLL5G_CFG0(), |r| {
                r.set_core_clk_div(3);
            })?;
        }
        self.modify(ANA_AC_POL().COMMON_SDLB().DLB_CTRL(), |r| {
            r.set_clk_period_01ns(36);
        })?;
        self.modify(ANA_AC_POL().COMMON_BDLB().DLB_CTRL(), |r| {
            r.set_clk_period_01ns(36);
        })?;
        self.modify(ANA_AC_POL().COMMON_BUM_SLB().DLB_CTRL(), |r| {
            r.set_clk_period_01ns(36);
        })?;
        self.modify(ANA_AC_POL().POL_ALL_CFG().POL_UPD_INT_CFG(), |r| {
            r.set_pol_upd_int(693);
        })?;
        self.modify(LRN().COMMON().AUTOAGE_CFG_1(), |r| {
            r.set_clk_period_01ns(36);
        })?;
        for i in 0..2 {
            self.modify(DEVCPU_GCB().SIO_CTRL(i).SIO_CLOCK(), |r| {
                r.set_sys_clk_period(36);
            })?;
        }
        self.modify(HSCH().HSCH_MISC().SYS_CLK_PER(), |r| {
            r.set_sys_clk_per_100ps(36);
        })?;
        self.modify(VOP().COMMON().LOC_CTRL(), |r| {
            r.set_loc_base_tick_cnt(28);
        })?;
        self.modify(AFI().TTI_TICKS().TTI_TICK_BASE(), |r| {
            r.set_base_len(14444);
        })?;

        Ok(())
    }

    /// Configures bandwidth to the given port.  The configuration must
    /// be applied with [apply_calendar] after all ports have been configured.
    fn set_calendar_bandwidth(
        &self,
        port: u8,
        bw: Bandwidth,
    ) -> Result<(), VscError> {
        self.modify(QSYS().CALCFG().CAL_AUTO(port / 16), |r| {
            let shift = (port % 16) * 2;
            let mut v = r.cal_auto();
            v &= !(0b11 << shift);
            v |= match bw {
                Bandwidth::None => 0b00,
                Bandwidth::Bw1G => 0b01,
                Bandwidth::Bw2G5 => 0b10,
                Bandwidth::Bw10G => 0b11,
            } << shift;
            r.set_cal_auto(v);
        })?;
        Ok(())
    }

    /// Applies the port configuration from repeated calls to
    /// [set_calendar_bandwidth].  Returns an error if the total bandwidth
    /// exceeds the chip's limit of 84 Gbps, or if communication to the
    /// chip fails in some other way.
    fn apply_calendar(&self) -> Result<(), VscError> {
        let mut total_bw_mhz = 0;
        for i in 0..4 {
            let d = self.read(QSYS().CALCFG().CAL_AUTO(i))?.cal_auto();
            for j in 0..16 {
                let v = (d >> (j * 2)) & 0b11;
                let bw = match v {
                    0b00 => Bandwidth::None,
                    0b01 => Bandwidth::Bw1G,
                    0b10 => Bandwidth::Bw2G5,
                    0b11 => Bandwidth::Bw10G,
                    _ => unreachable!(),
                };
                total_bw_mhz += bw.bandwidth_mhz();
            }
        }

        // The chip nominally has 80 Gbps of bandwidth, but the SDK checks
        // against 84 Gbps.  Perhaps this is because we overclock the PLLs;
        // the datasheet mentions that this allows us to exceed 80 Gbps,
        // but doesn't specify exactly how much.
        if total_bw_mhz > 84_000 {
            return Err(VscError::TooMuchBandwidth(total_bw_mhz));
        }

        // "672->671, BZ19678"
        self.modify(QSYS().CALCFG().CAL_CTRL(), |r| {
            r.set_cal_auto_grant_rate(671);
        })?;

        // The SDK configures HSCH:HSCH_MISC.OUTB_SHARE_ENA here, but we're
        // not using CPU ports, so we can skip it

        // Configure CAL_CTRL to use the CAL_AUTO settings
        self.modify(QSYS().CALCFG().CAL_CTRL(), |r| {
            r.set_cal_mode(8);
        })?;

        // Confirm that the config was applied
        if self.read(QSYS().CALCFG().CAL_CTRL())?.cal_auto_error() == 1 {
            Err(VscError::CalConfigFailed)
        } else {
            Ok(())
        }
    }

    /// Configures the VLAN for debugging and bringup.  This is pretty
    /// specific to hardware set up on Matt's desk, but shows all of the
    /// pieces working together.
    ///
    /// This mode expects the following configuration for a VSC7448 dev kit:
    /// - Router connected on port 1
    /// - NUC attached on port 3
    /// - Management network dev board on port 51 (SGMII)
    ///
    /// It configures two VLANs:
    /// - VID 1 allows communication on all ports
    /// - VID 0x133 allows communication between the NUC and the mgmt dev board
    ///
    /// Untagged packets on port 1 and 3 become members of VLAN 1.
    ///
    /// Untagged packets on port 51 become members of VLAN 0x133.  Note that
    /// this means packets from the management network dev kit can't make their
    /// way back to the router, so pinging the board from a laptop won't work!
    ///
    /// Invalid tagged packets are dropped.
    pub fn configure_vlan_optional(&self) -> Result<(), VscError> {
        // Enable the VLAN
        self.write_with(ANA_L3().COMMON().VLAN_CTRL(), |r| r.set_vlan_ena(1))?;

        // By default, there are three VLANs configured in ANA_L3:
        // 0, 1, and 4095.  We disable 0 and 4095, and leave 1 running (since
        // it's the default VLAN)
        for vid in [0, 4095] {
            self.write_port_mask(ANA_L3().VLAN(vid).VLAN_MASK_CFG(), 0)?;
        }

        // Configure VID 0x133 to include the NUC and mgmt dev board
        self.write_port_mask(
            ANA_L3().VLAN(0x133).VLAN_MASK_CFG(),
            (1 << 3) | // NUC
            (1 << 51), // mgmt dev board
        )?;

        // Configure the uplink and NUC port
        for p in [1, 3] {
            let port = ANA_CL().PORT(p);
            self.modify(port.VLAN_CTRL(), |r| {
                // All ports are on the default VID (0x1) at boot
                r.set_vlan_pop_cnt(1);
                r.set_vlan_aware_ena(1);
            })?;

            // Accept TPID 0x8100 and untagged or one-tag frames
            self.modify(port.VLAN_TPID_CTRL(), |r| {
                r.set_basic_tpid_aware_dis(0b1110);
                r.set_rt_tag_ctrl(0b0011);
            })?;
        }

        // Configure management network dev kit port as VLAN aware
        let port = ANA_CL().PORT(51);
        self.modify(port.VLAN_CTRL(), |r| {
            r.set_vlan_aware_ena(1);
            r.set_port_vid(0x133);
        })?;
        self.modify(port.VLAN_TPID_CTRL(), |r| {
            r.set_basic_tpid_aware_dis(0b1110);
            r.set_rt_tag_ctrl(0b0011);
        })?;

        // Configure VLAN ingress filtering, so packets that arrive and
        // aren't part of an appropriate VLAN are dropped.  This occurs
        // after VLAN classification, so the downstream ports that have
        // frames classified on ingress should work.
        self.write_port_mask(
            ANA_L3().COMMON().VLAN_FILTER_CTRL(),
            (1 << 53) - 1,
        )?;

        Ok(())
    }

    /// Implements the VLAN scheme described in RFD 250.
    pub fn configure_vlan_strict(&self) -> Result<(), VscError> {
        const UPLINK: u8 = 49; // DEV10G_0, uplink to the Tofino 2

        // Enable the VLAN
        self.write_with(ANA_L3().COMMON().VLAN_CTRL(), |r| r.set_vlan_ena(1))?;

        // By default, there are three VLANs configured in ANA_L3:
        // 0, 1, and 4095.  We disable all of them, since we only want to
        // allow very specific VIDs.
        for vid in [0, 1, 4095] {
            self.write_port_mask(ANA_L3().VLAN(vid).VLAN_MASK_CFG(), 0)?;
        }

        // Configure the downstream ports, which each have their own VLANs
        for p in (0..=52).filter(|p| *p != UPLINK) {
            let port = ANA_CL().PORT(p);

            // Configure the 0x1YY VLAN for this port
            self.write_port_mask(
                ANA_L3().VLAN(0x100 + p as u16).VLAN_MASK_CFG(),
                (1 << p) | (1 << UPLINK),
            )?;

            // The downstream ports expect untagged frames, and classify
            // them based on a per-port VID assigned here.
            self.modify(port.VLAN_CTRL(), |r| {
                r.set_port_vid(0x100 + p as u32);
                r.set_vlan_aware_ena(1);
            })?;
            // Accept no TPIDs, and only route untagged frames.
            self.modify(port.VLAN_TPID_CTRL(), |r| {
                r.set_basic_tpid_aware_dis(0b1111);
                r.set_rt_tag_ctrl(0b0001);
            })?;
        }

        // The uplink port requires one VLAN tag, and pops it on ingress
        //
        // It has a default VID of 0x1, but we removed all ports from
        // that VLAN, so it will only accept our desired set of VIDs.
        let port = ANA_CL().PORT(UPLINK);
        self.modify(port.VLAN_CTRL(), |r| {
            r.set_vlan_pop_cnt(1);
            r.set_vlan_aware_ena(1);
        })?;
        // Only accept 0x8100 as a valid TPID, to keep things simple,
        // and only route frames with one accepted tag
        self.modify(port.VLAN_TPID_CTRL(), |r| {
            r.set_basic_tpid_aware_dis(0b1110);
            r.set_rt_tag_ctrl(0b0010);
        })?;
        // Discard frames with < 1 tag
        self.modify(port.VLAN_FILTER_CTRL(0), |r| {
            r.set_tag_required_ena(1);
        })?;
        let rew = REW().PORT(UPLINK);
        // Use the rewriter to tag all frames on egress from the upstream port
        // (using the VID assigned on ingress into a downstream port)
        self.modify(rew.TAG_CTRL(), |r| {
            r.set_tag_cfg(1);
        })?;

        // Configure VLAN ingress filtering, so packets that arrive and
        // aren't part of an appropriate VLAN are dropped.  This occurs
        // after VLAN classification, so the downstream ports that have
        // frames classified on ingress should work.
        self.write_port_mask(
            ANA_L3().COMMON().VLAN_FILTER_CTRL(),
            (1 << 53) - 1,
        )?;

        Ok(())
    }
}

enum Bandwidth {
    None,
    Bw1G,
    Bw2G5,
    Bw10G,
}

impl Bandwidth {
    fn bandwidth_mhz(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Bw1G => 1_000,
            Self::Bw2G5 => 2_500,
            Self::Bw10G => 10_000,
        }
    }
}

/// Sets the frequency of the reference clock.  The specific values are based
/// on the REFCLK_SEL pins.
#[derive(Copy, Clone)]
pub enum RefClockFreq {
    Clk25MHz = 0b100,
    Clk125MHz = 0b000,
    Clk156p25MHz = 0b001,
}
