use crate::{vsc7448_spi::Vsc7448Spi, VscError};
use ringbuf::*;
use userlib::*;
use vsc7448_pac::{phy, Vsc7448};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Initialized(u64),
    FailedToInitialize(VscError),
    PhyScanError { miim: u8, phy: u8, err: VscError },
    PhyLinkChanged { port: u8, status: u16 },
}
ringbuf!(Trace, 16, Trace::None);

pub struct Bsp<'a> {
    vsc7448: &'a Vsc7448Spi,
}
impl<'a> Bsp<'a> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448Spi) -> Result<Self, VscError> {
        let out = Bsp { vsc7448 };
        out.init()?;
        Ok(out)
    }

    /// Attempts to initialize the system.  This is based on a VSC7448 dev kit
    /// (VSC5627EV), so will need to change depending on your system.
    fn init(&self) -> Result<(), VscError> {
        // We call into an inner function so that we can easily match on
        // errors here and log in the ringbuf.
        let out = self.init_inner();
        match out {
            Err(e) => ringbuf_entry!(Trace::FailedToInitialize(e)),
            Ok(_) => ringbuf_entry!(Trace::Initialized(sys_get_timer().now)),
        }
        out
    }
    fn init_inner(&self) -> Result<(), VscError> {
        // We assume that the only person running on a gemini-bu-1 is Matt, who is
        // talking to a VSC7448 dev kit on his desk.  In this case, we want to
        // configure the GPIOs to allow MIIM1 and 2 to be active, by setting
        // GPIO_56-59 to Overlaid Function 1
        self.vsc7448.write(
            Vsc7448::DEVCPU_GCB().GPIO().GPIO_ALT1(0),
            0xF000000.into(),
        )?;

        // Based on `jr2_init_conf_set`
        self.vsc7448.modify(
            Vsc7448::ANA_AC().STAT_GLOBAL_CFG_PORT().STAT_RESET(),
            |r| r.set_reset(1),
        )?;
        self.vsc7448.modify(Vsc7448::ASM().CFG().STAT_CFG(), |r| {
            r.set_stat_cnt_clr_shot(1)
        })?;
        self.vsc7448
            .modify(Vsc7448::QSYS().RAM_CTRL().RAM_INIT(), |r| {
                r.set_ram_init(1);
                r.set_ram_ena(1);
            })?;
        self.vsc7448
            .modify(Vsc7448::REW().RAM_CTRL().RAM_INIT(), |r| {
                r.set_ram_init(1);
                r.set_ram_ena(1);
            })?;
        // The VOP isn't in the datasheet, but it's in the SDK
        self.vsc7448
            .modify(Vsc7448::VOP().RAM_CTRL().RAM_INIT(), |r| {
                r.set_ram_init(1);
                r.set_ram_ena(1);
            })?;
        self.vsc7448
            .modify(Vsc7448::ANA_AC().RAM_CTRL().RAM_INIT(), |r| {
                r.set_ram_init(1);
                r.set_ram_ena(1);
            })?;
        self.vsc7448
            .modify(Vsc7448::ASM().RAM_CTRL().RAM_INIT(), |r| {
                r.set_ram_init(1);
                r.set_ram_ena(1);
            })?;
        self.vsc7448
            .modify(Vsc7448::DSM().RAM_CTRL().RAM_INIT(), |r| {
                r.set_ram_init(1);
                r.set_ram_ena(1);
            })?;

        hl::sleep_for(1);
        // TODO: read back all of those autoclear bits and make sure they cleared

        // Enable the queue system
        self.vsc7448
            .write_with(Vsc7448::QSYS().SYSTEM().RESET_CFG(), |r| {
                r.set_core_ena(1)
            })?;

        // Based on `jr2_init_ana`
        self.vsc7448.modify(
            Vsc7448::ANA_AC_POL().POL_ALL_CFG().POL_ALL_CFG(),
            |r| {
                r.set_storm_force_init(1);
                r.set_prio_force_init(1);
                r.set_acl_force_init(1);
                r.set_force_init(1);
            },
        )?;
        // TODO: this is where we can configure stats

        // Spin until the chip autoclears all of the one-shot bits from above
        // Note that we don't check PRIO_FORCE_INIT, which experimentally
        // doesn't get autocleared (?); this matches the behavior in the SDK,
        // but seems... odd.
        {
            let mut ready = false;
            for _i in 0..1000 {
                let r = self
                    .vsc7448
                    .read(Vsc7448::ANA_AC_POL().POL_ALL_CFG().POL_ALL_CFG())?;
                if r.storm_force_init() == 0
                    && r.acl_force_init() == 0
                    && r.force_init() == 0
                {
                    ready = true;
                    break;
                }
                hl::sleep_for(1);
            }
            if !ready {
                return Err(VscError::AnaCfgTimeout);
            }
        }

        // TODO: The SDK configures VAUI stuff here using
        // `vtss_jr2_vaui_lane_alignement`.  This will matter for the 10G port
        // bringup later.

        // Quoth the SDK in `vtss_jr2_port_init`:
        // "Gotta disable ingress and egress queue limitation, because directed
        //  frames transmitted from the CPU could cause the feature to wrongly
        //  think the queue has reached its limit after a link down-up."
        //
        //  and
        //
        // "Make sure the ports are not VStaX aware, because that will cause the
        //  switch to move a possible VStaX header from the frame into the IFH.
        //  This is not NPI-port compatible."
        for port in 0..53 {
            self.vsc7448.modify(
                Vsc7448::XQS().QLIMIT_PORT(port).QLIMIT_DIS_CFG(),
                |r| {
                    r.set_qlimit_igr_dis(1);
                    r.set_qlimit_egr_dis(1);
                },
            )?;
            self.vsc7448
                .modify(Vsc7448::ASM().CFG().PORT_CFG(port), |r| {
                    r.set_vstax2_awr_ena(0);
                })?;
        }

        // Compared to the SDK, we're skipping `jr2_sgpio_init` since we're
        // not using IRQs, and `jr2_packet_init`, since we're not doing packet
        // injection.

        // The code below is based on `jr2_l2_init`, to initialize the L2 and
        // VPN functionality.
        self.vsc7448
            .write_with(Vsc7448::ANA_L3().COMMON().VLAN_CTRL(), |r| {
                r.set_vlan_ena(1)
            })?;
        // Clear VID 4095 mask, for some reason!
        self.vsc7448
            .write_port_mask(Vsc7448::ANA_L3().VLAN(4095).VLAN_MASK_CFG(), 0)?;

        // Do some per-port configuration that I don't really understand
        for port in 0..53 {
            let port = Vsc7448::ANA_CL().PORT(port);
            self.vsc7448.modify(
                port.VLAN_TPID_CTRL(),
                |r| r.set_basic_tpid_aware_dis(0x7fff), // ???
            )?;
            // Discard frames with multicast SMAC address
            self.vsc7448
                .modify(port.FILTER_CTRL(), |r| r.set_filter_smac_mc_dis(0))?;
        }

        // Enable forwarding and learning
        const NUM_MSTP_TABLES: u32 = 66;
        const PORT_MASK_ALL: u64 = (1 << 53) - 1;
        for msti in 0..NUM_MSTP_TABLES {
            // Enable all ports for MSTP table 0, and no ports for the others
            // TODO: is this necessary??
            let mask = if msti == 0 { PORT_MASK_ALL } else { 0 };
            self.vsc7448.write_port_mask(
                Vsc7448::ANA_L3().MSTP(msti).MSTP_FWD_CFG(),
                mask,
            )?;
            self.vsc7448.write_port_mask(
                Vsc7448::ANA_L3().MSTP(msti).MSTP_LRN_CFG(),
                mask,
            )?;
        }
        self.vsc7448.write_port_mask(
            Vsc7448::ANA_L3().COMMON().PORT_FWD_CTRL(),
            PORT_MASK_ALL,
        )?;
        self.vsc7448.write_port_mask(
            Vsc7448::ANA_L3().COMMON().PORT_LRN_CTRL(),
            PORT_MASK_ALL,
        )?;

        // "Disable learning in VLAN 0 (used for EVCs)" ??
        self.vsc7448
            .modify(Vsc7448::ANA_L3().VLAN(0).VLAN_CFG(), |r| {
                r.set_vlan_lrn_dis(0)
            })?;

        // The SDK now configures alternate S-tag ethernet type and MAC aging;
        // let's skip both of those steps for now.

        // Enable frame aging
        self.vsc7448
            .write_with(Vsc7448::QSYS().SYSTEM().FRM_AGING(), |r| {
                // This is in units of 4 us, so this is 0.5 seconds
                r.set_max_age(500000 / 4)
            })?;

        // Disable advanced (VSTAX) learning (?)
        self.vsc7448
            .modify(Vsc7448::ANA_L2().COMMON().LRN_CFG(), |r| {
                r.set_vstax_basic_lrn_mode_ena(1)
            })?;

        // This concludes the code based on `vtss_jr2_l2_init`!

        // The VSC7448 dev kit has 2x VSC8522 PHYs on each of MIIM1 and MIIM2.
        // Each PHYs on the same MIIM bus is strapped to different ports.
        hl::sleep_for(105); // Minimum time between reset and SMI access
        for miim in [1, 2] {
            self.vsc7448.modify(
                Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_CFG(),
                |cfg| cfg.set_miim_cfg_prescale(0xFF),
            )?;
            // We only need to check this on one PHY port per physical PHY
            // chip.  Port 0 maps to one PHY chip, and port 12 maps to the
            // other one (controlled by hardware pull-ups).
            for phy in [0, 12] {
                // Do a self-reset on the PHY
                self.vsc7448.phy_modify(
                    miim,
                    phy,
                    phy::STANDARD::MODE_CONTROL(),
                    |g| g.set_sw_reset(1),
                )?;
                let id1 = self
                    .vsc7448
                    .phy_read(miim, phy, phy::STANDARD::IDENTIFIER_1())?
                    .0;
                if id1 != 0x7 {
                    return Err(VscError::BadPhyId1(id1));
                }
                let id2 = self
                    .vsc7448
                    .phy_read(miim, phy, phy::STANDARD::IDENTIFIER_2())?
                    .0;
                if id2 != 0x6f3 {
                    return Err(VscError::BadPhyId2(id2));
                }

                // Disable COMA MODE, which keeps the chip holding itself in reset
                self.vsc7448.phy_modify(
                    miim,
                    phy,
                    phy::GPIO::GPIO_CONTROL_2(),
                    |g| g.set_coma_mode_output_enable(0),
                )?;

                // Configure the PHY in QSGMII + 12 port mode
                self.vsc7448.phy_write(
                    miim,
                    phy,
                    phy::GPIO::MICRO_PAGE(),
                    0x80A0.into(),
                )?;
            }
        }

        // I want to configure ports 0-3 (or 1-4, depending on numbering) on
        // the VSC7448 to use QSGMII to talk on SERDES6G_4 to the VSC8522.
        //
        // The following code is based on port_setup in the MESA SDK, but
        // extracted and trimmed down to the bare necessacities (e.g. assuming
        // the chip is configured from reset)
        self.vsc7448
            .modify(Vsc7448::HSIO().HW_CFGSTAT().HW_CFG(), |r| {
                // Enable QSGMII mode for devices DEV1G_0, DEV1G_1, DEV1G_2, and
                // DEV1G_3 via SerDes6G_4.
                let ena = r.qsgmii_ena() | 1;
                r.set_qsgmii_ena(ena);
            })?;
        for port in 0..4 {
            // Reset the PCS TX clock domain.  In the SDK, this is accompanied
            // by the cryptic comment "BZ23738", which may refer to an errata
            // of some kind?
            self.vsc7448.modify(
                Vsc7448::DEV1G(port).DEV_CFG_STATUS().DEV_RST_CTRL(),
                |r| {
                    r.set_pcs_tx_rst(0);
                },
            )?;
        }
        // Configure SERDES6G_4 for QSGMII
        // Based on jr2_sd6g_cfg in vtss_jaguar2_serdes
        const SERDES6G_INSTANCE: u16 = 4;
        self.serdes6g_read(SERDES6G_INSTANCE)?;
        let ana_cfg = Vsc7448::HSIO().SERDES6G_ANA_CFG();
        let dig_cfg = Vsc7448::HSIO().SERDES6G_DIG_CFG();
        self.vsc7448
            .modify(ana_cfg.SERDES6G_COMMON_CFG(), |r| r.set_sys_rst(0))?;
        self.vsc7448
            .modify(dig_cfg.SERDES6G_MISC_CFG(), |r| r.set_lane_rst(1))?;
        self.serdes6g_write(SERDES6G_INSTANCE)?;

        self.vsc7448
            .modify(ana_cfg.SERDES6G_OB_CFG(), |r| r.set_ob_ena1v_mode(0))?;
        self.vsc7448
            .modify(ana_cfg.SERDES6G_OB_CFG(), |r| r.set_ob_ena1v_mode(0))?;
        self.vsc7448.modify(ana_cfg.SERDES6G_IB_CFG(), |r| {
            r.set_ib_reg_pat_sel_offset(0)
        })?;
        // Skip configuration related to VTSS_PORT_LB_FACILITY/EQUIPMENT
        self.vsc7448.modify(ana_cfg.SERDES6G_PLL_CFG(), |r| {
            r.set_pll_fsm_ctrl_data(120)
        })?;
        self.vsc7448.modify(ana_cfg.SERDES6G_COMMON_CFG(), |r| {
            r.set_sys_rst(1);
            r.set_ena_lane(1);
            r.set_qrate(0);
            r.set_if_mode(3);
        })?;
        self.serdes6g_write(SERDES6G_INSTANCE)?;

        // Enable the PLL then wait 20 ms for bringup
        self.vsc7448
            .modify(ana_cfg.SERDES6G_PLL_CFG(), |r| r.set_pll_fsm_ena(1))?;
        self.serdes6g_write(SERDES6G_INSTANCE)?;
        hl::sleep_for(20);

        // Start IB calibration, then wait 60 ms for it to complete
        self.vsc7448
            .modify(ana_cfg.SERDES6G_IB_CFG(), |r| r.set_ib_cal_ena(1))?;
        self.vsc7448
            .modify(dig_cfg.SERDES6G_MISC_CFG(), |r| r.set_lane_rst(0))?;
        self.serdes6g_write(SERDES6G_INSTANCE)?;
        hl::sleep_for(60);

        // "Set ib_tsdet and ib_reg_pat_sel_offset back to correct value"
        // (according to the SDK)
        self.vsc7448.modify(ana_cfg.SERDES6G_IB_CFG(), |r| {
            r.set_ib_reg_pat_sel_offset(0);
            r.set_ib_sig_det_clk_sel(7);
        })?;
        self.vsc7448
            .modify(ana_cfg.SERDES6G_IB_CFG1(), |r| r.set_ib_tsdet(3))?;
        self.serdes6g_write(SERDES6G_INSTANCE)?;

        for port in 0..4 {
            self.port1g_flush(port)?;

            // Enable full duplex mode and GIGA SPEED
            let dev = Vsc7448::DEV1G(port as u32);
            self.vsc7448
                .modify(dev.MAC_CFG_STATUS().MAC_MODE_CFG(), |r| {
                    r.set_fdx_ena(1);
                    r.set_giga_mode_ena(1);
                })?;

            self.vsc7448
                .modify(dev.MAC_CFG_STATUS().MAC_IFG_CFG(), |r| {
                    // NOTE: these are speed-dependent options and aren't
                    // fully documented in the manual; this values are chosen
                    // based on the SDK for 1G, full duplex operation.
                    r.set_tx_ifg(4);
                    r.set_rx_ifg1(0);
                    r.set_rx_ifg2(0);
                })?;

            // The upcoming steps depend on how the port is talking to the
            // outside world (100FX / SGMII / SERDES).  In this case, the port
            // is talking over QSGMII, which is configured like SGMII then
            // combined in the macro block (I may be butchering some details of
            // terminology or architecture here).

            // The device is configured to SGMII mode by default, so no
            // changes are needed there.

            // This bit isn't documented in the datasheet, but the SDK says it
            // must be set in SGMII mode.  It allows a link to be set up by
            // software, even if autonegotiation fails.
            self.vsc7448
                .modify(dev.PCS1G_CFG_STATUS().PCS1G_ANEG_CFG(), |r| {
                    r.set_sw_resolve_ena(1)
                })?;

            // Configure signal detect line with values from the dev kit
            // This is dependent on the port setup.
            self.vsc7448.modify(
                dev.PCS1G_CFG_STATUS().PCS1G_SD_CFG(),
                |r| {
                    // TODO: why bother configuring sd_sel and sd_pol if you're
                    // just going to ignore the signal detect line?
                    r.set_sd_sel(1); // External signal_detect line
                    r.set_sd_pol(0); // Active low
                    r.set_sd_ena(0); // Ignored
                },
            )?;

            // Enable the PCS!
            self.vsc7448
                .modify(dev.PCS1G_CFG_STATUS().PCS1G_CFG(), |r| {
                    r.set_pcs_ena(1)
                })?;

            // Set max length based on VTSS_MAX_FRAME_LENGTH_MAX; this is how
            // it's configured in a running SDK.
            //
            // TODO: check if this is necessary, since the default is to not
            // check frame lengths (MAC_ADV_CHECK_CFG = 0)
            self.vsc7448
                .modify(dev.MAC_CFG_STATUS().MAC_MAXLEN_CFG(), |r| {
                    r.set_max_len(0x2800)
                })?;

            // The SDK configures MAC VLAN awareness here; let's not do that
            // for the time being.

            // TODO: the SDK also configures flow control (`jr2_port_fc_setup`)
            // and policer flow control (`vtss_jr2_port_policer_fc_set`) around
            // here; is that necessary?

            // Turn on the MAC!
            self.vsc7448.write_with(
                dev.MAC_CFG_STATUS().MAC_ENA_CFG(),
                |r| {
                    r.set_tx_ena(1);
                    r.set_rx_ena(1);
                },
            )?;

            // Take MAC, Port, Phy (intern), and PCS (SGMII) clocks out of
            // reset, turning on a 1G port data rate.
            self.vsc7448
                .write_with(dev.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
                    r.set_speed_sel(2)
                })?;

            self.vsc7448.modify(
                Vsc7448::QFWD().SYSTEM().SWITCH_PORT_MODE(port as u32),
                |r| {
                    r.set_port_ena(1);
                    r.set_fwd_urgency(104); // This is different based on speed
                },
            )?;
        }

        Ok(())
    }

    /// Flushes a particular 1G port.  This is equivalent to `jr2_port_flush`
    /// in the MESA toolkit.
    fn port1g_flush(&self, port: u8) -> Result<(), VscError> {
        // 1: Reset the PCS Rx clock domain
        let dev = Vsc7448::DEV1G(port as u32);
        self.vsc7448
            .modify(dev.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
                r.set_pcs_rx_rst(1)
            })?;

        // 2: Reset the PCS Rx clock domain
        self.vsc7448
            .modify(dev.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| r.set_rx_ena(0))?;

        // 3: Disable traffic being sent to or from switch port
        self.vsc7448.modify(
            Vsc7448::QFWD().SYSTEM().SWITCH_PORT_MODE(port as u32),
            |r| r.set_port_ena(0),
        )?;

        // 4: Disable dequeuing from the egress queues
        self.vsc7448.modify(
            Vsc7448::HSCH().HSCH_MISC().PORT_MODE(port as u32),
            |r| r.set_dequeue_dis(1),
        )?;

        // 5: Disable Flowcontrol
        self.vsc7448.modify(
            Vsc7448::QSYS().PAUSE_CFG().PAUSE_CFG(port as u32),
            |r| r.set_pause_ena(0),
        )?;

        // 5.1: Disable PFC
        self.vsc7448.modify(
            Vsc7448::QRES().RES_QOS_ADV().PFC_CFG(port as u32),
            |r| r.set_tx_pfc_ena(0),
        )?;

        // 6: Wait a worst case time 8ms (jumbo/10Mbit)
        hl::sleep_for(8);

        // 7: Flush the queues accociated with the port
        self.vsc7448
            .modify(Vsc7448::HSCH().HSCH_MISC().FLUSH_CTRL(), |r| {
                r.set_flush_port(port as u32);
                r.set_flush_dst(1);
                r.set_flush_src(1);
                r.set_flush_ena(1);
            })?;

        // 8: Enable dequeuing from the egress queues
        self.vsc7448.modify(
            Vsc7448::HSCH().HSCH_MISC().PORT_MODE(port as u32),
            |r| r.set_dequeue_dis(0),
        )?;

        // 9: Wait until flushing is complete
        self.port_flush_wait(port)?;

        // 10: Reset the MAC clock domain
        self.vsc7448
            .modify(dev.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
                r.set_pcs_rx_rst(0);
                r.set_pcs_tx_rst(0);
                r.set_mac_rx_rst(1);
                r.set_mac_tx_rst(1);
                r.set_speed_sel(3);
            })?;

        // 11: Clear flushing
        self.vsc7448
            .modify(Vsc7448::HSCH().HSCH_MISC().FLUSH_CTRL(), |r| {
                r.set_flush_ena(0);
            })?;
        Ok(())
    }

    /// Waits for a port flush to finish.  This is based on
    /// `jr2_port_flush_poll` in the MESA SDK
    fn port_flush_wait(&self, port: u8) -> Result<(), VscError> {
        for _ in 0..32 {
            let mut empty = true;
            // DST-MEM and SRC-MEM
            for base in [0, 2048] {
                for prio in 0..8 {
                    let value = self.vsc7448.read(
                        Vsc7448::QRES()
                            .RES_CTRL(base + 8 * port as u32 + prio)
                            .RES_STAT(),
                    )?;
                    empty &= value.maxuse() == 0;
                    // Keep looping, because these registers are clear-on-read,
                    // so it's more efficient to read them all, even if we know
                    // that the port isn't currently empty.
                }
            }
            if empty {
                return Ok(());
            }
            hl::sleep_for(1);
        }
        return Err(VscError::PortFlushTimeout { port });
    }

    /// Reads from a specific SERDES6G instance, which is done by writing its
    /// value (as a bitmask) to a particular register with a read flag set,
    /// then waiting for the flag to autoclear.
    fn serdes6g_read(&self, instance: u16) -> Result<(), VscError> {
        let mut reg: vsc7448_pac::hsio::mcb_serdes6g_cfg::MCB_SERDES6G_ADDR_CFG =
            0.into();
        reg.set_serdes6g_rd_one_shot(1);
        reg.set_serdes6g_addr(1 << instance);
        let addr = Vsc7448::HSIO().MCB_SERDES6G_CFG().MCB_SERDES6G_ADDR_CFG();
        self.vsc7448.write(addr, reg)?;
        for _ in 0..32 {
            if self.vsc7448.read(addr)?.serdes6g_rd_one_shot() != 1 {
                return Ok(());
            }
        }
        return Err(VscError::Serdes6gReadTimeout { instance });
    }

    /// Reads from a specific SERDES6G instance, which is done by writing its
    /// value (as a bitmask) to a particular register with a read flag set,
    /// then waiting for the flag to autoclear.
    fn serdes6g_write(&self, instance: u16) -> Result<(), VscError> {
        let mut reg: vsc7448_pac::hsio::mcb_serdes6g_cfg::MCB_SERDES6G_ADDR_CFG =
            0.into();
        reg.set_serdes6g_wr_one_shot(1);
        reg.set_serdes6g_addr(1 << instance);
        let addr = Vsc7448::HSIO().MCB_SERDES6G_CFG().MCB_SERDES6G_ADDR_CFG();
        self.vsc7448.write(addr, reg)?;
        for _ in 0..32 {
            if self.vsc7448.read(addr)?.serdes6g_wr_one_shot() != 1 {
                return Ok(());
            }
        }
        return Err(VscError::Serdes6gWriteTimeout { instance });
    }

    pub fn run(&self) -> ! {
        let mut link_up = [[false; 24]; 2];
        loop {
            hl::sleep_for(100);
            for miim in [1, 2] {
                for phy in 0..24 {
                    match self.vsc7448.phy_read(
                        miim,
                        phy,
                        phy::STANDARD::MODE_STATUS(),
                    ) {
                        Ok(status) => {
                            let up = (status.0 & (1 << 5)) != 0;
                            if up != link_up[miim as usize - 1][phy as usize] {
                                link_up[miim as usize - 1][phy as usize] = up;
                                ringbuf_entry!(Trace::PhyLinkChanged {
                                    port: (miim - 1) * 24 + phy,
                                    status: status.0,
                                });
                            }
                        }
                        Err(err) => {
                            ringbuf_entry!(Trace::PhyScanError {
                                miim,
                                phy,
                                err
                            })
                        }
                    }
                }
            }
        }
    }
}
