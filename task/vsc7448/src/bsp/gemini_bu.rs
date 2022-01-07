use crate::{serdes10g::SerdesConfig, vsc7448_spi::Vsc7448Spi, VscError};
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

    /// Initializes four ports on front panel RJ45 connectors
    fn init_rj45(&self) -> Result<(), VscError> {
        // The VSC7448 dev kit has 2x VSC8522 PHYs on each of MIIM1 and MIIM2.
        // Each PHYs on the same MIIM bus is strapped to different ports.
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
                    r.set_sd_ena(0); // Ignored
                },
            )?;

            // Enable the PCS!
            self.vsc7448
                .modify(dev.PCS1G_CFG_STATUS().PCS1G_CFG(), |r| {
                    r.set_pcs_ena(1)
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

    /// Initializes two ports on front panel SFP+ connectors
    fn init_sfp(&self) -> Result<(), VscError> {
        //  Now, let's bring up one SFP+ port
        //
        //  SFP ports A and B are connected to S33/34 using SFI.  We need to
        //  bring up 10G SERDES then enable the ports
        //
        //  If we want to actually read from the SFI EEPROMs, we'd do that
        //  over I2C.  There's also serial GPIO for various other SFP
        //  functions, e.g. RATESEL, LOS, TXDISABLE, and more!  For now, let's
        //  see if we can bring up the port without doing any of that.

        //  I2C_SDA = GPIO15_TWI_SDA on the VSC7448 (alt "01")
        self.vsc7448.write(
            Vsc7448::DEVCPU_GCB().GPIO().GPIO_ALT(0),
            0x00008000.into(),
        )?;

        //  I2C_SCL = GPIO17_SI_nCS3 (for port A)
        //            GPIO18_SI_nCS3 (for port B)
        //            (both alt "10")
        self.vsc7448.write(
            Vsc7448::DEVCPU_GCB().GPIO().GPIO_ALT(1),
            0x00060000.into(),
        )?;

        // HW_CFG is already set up for 10G on all four DEV10G

        let serdes_cfg = SerdesConfig::new()?;
        for (port, dev) in [(49, 0), (50, 1)] {
            // jr2_sd10g_xfi_mode
            self.vsc7448.modify(
                Vsc7448::XGXFI(dev).XFI_CONTROL().XFI_MODE(),
                |r| {
                    r.set_sw_rst(0);
                    r.set_endian(1);
                    r.set_sw_ena(1);
                },
            )?;

            // jr2_sd10g_cfg, moved into a separate function because bringing
            // up a 10G SERDES is _hard_
            serdes_cfg.apply(dev, self.vsc7448)?;
            self.port10g_flush(dev as u8, port)?;

            // Remaining logic is from `jr2_port_conf_10g_set`
            // Handle signal detect
            let dev = Vsc7448::DEV10G(dev);
            self.vsc7448.modify(
                dev.PCS_XAUI_CONFIGURATION().PCS_XAUI_SD_CFG(),
                |r| {
                    r.set_sd_ena(0);
                },
            )?;
            // Enable SFI PCS
            self.vsc7448.modify(
                dev.PCS_XAUI_CONFIGURATION().PCS_XAUI_CFG(),
                |r| {
                    r.set_pcs_ena(1);
                },
            )?;
            self.vsc7448
                .modify(dev.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| {
                    r.set_rx_ena(1);
                    r.set_tx_ena(1);
                })?;
            self.vsc7448
                .modify(dev.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
                    r.set_pcs_rx_rst(0);
                    r.set_pcs_tx_rst(0);
                    r.set_mac_rx_rst(0);
                    r.set_mac_tx_rst(0);
                    r.set_speed_sel(7); // SFI
                })?;
            self.vsc7448.modify(
                Vsc7448::QFWD().SYSTEM().SWITCH_PORT_MODE(port.into()),
                |r| {
                    r.set_port_ena(1);
                    r.set_port_ena(9);
                },
            )?;
        }

        Ok(())
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

        hl::sleep_for(105); // Minimum time between reset and SMI access
        self.init_rj45()?;
        self.init_sfp()?;

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

        self.port_flush_inner(port.into())?;

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

    /// Flushes a particular 10G port.  This is equivalent to `jr2_port_flush`
    /// in the MESA toolkit.  Unfortunately, it's mostly a copy-pasta from
    /// [port_1g_flush], because the registers have similar fields but are
    /// different types in our PAC crate.
    ///
    /// `dev` is the 10G device (0-4); `port` is the equivalent port
    fn port10g_flush(&self, dev: u8, port: u8) -> Result<(), VscError> {
        // 1: Reset the PCS Rx clock domain
        let dev10g = Vsc7448::DEV10G(dev.into());
        self.vsc7448
            .modify(dev10g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
                r.set_pcs_rx_rst(1)
            })?;

        // 2: Reset the PCS Rx clock domain
        self.vsc7448
            .modify(dev10g.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| {
                r.set_rx_ena(0)
            })?;

        self.port_flush_inner(port.into())?;

        // 10: Reset the MAC clock domain
        self.vsc7448
            .modify(dev10g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
                r.set_pcs_tx_rst(1);
                r.set_mac_rx_rst(1);
                r.set_mac_tx_rst(1);
                r.set_speed_sel(6);
            })?;

        // 11: Clear flushing
        self.vsc7448
            .modify(Vsc7448::HSCH().HSCH_MISC().FLUSH_CTRL(), |r| {
                r.set_flush_ena(0);
            })?;

        // Bonus for 10G ports: disable XAUI, RXAUI, SFI PCS
        self.vsc7448.modify(
            dev10g.PCS_XAUI_CONFIGURATION().PCS_XAUI_CFG(),
            |r| {
                r.set_pcs_ena(0);
            },
        )?;
        self.vsc7448.modify(
            dev10g.PCS2X6G_CONFIGURATION().PCS2X6G_CFG(),
            |r| {
                r.set_pcs_ena(0);
            },
        )?;
        self.vsc7448.modify(
            Vsc7448::PCS10G_BR(dev.into()).PCS_10GBR_CFG().PCS_CFG(),
            |r| {
                r.set_pcs_ena(0);
            },
        )?;

        Ok(())
    }

    /// Shared logic between 1G and 10G port flushing
    fn port_flush_inner(&self, port: u32) -> Result<(), VscError> {
        // 3: Disable traffic being sent to or from switch port
        self.vsc7448
            .modify(Vsc7448::QFWD().SYSTEM().SWITCH_PORT_MODE(port), |r| {
                r.set_port_ena(0)
            })?;

        // 4: Disable dequeuing from the egress queues
        self.vsc7448
            .modify(Vsc7448::HSCH().HSCH_MISC().PORT_MODE(port), |r| {
                r.set_dequeue_dis(1)
            })?;

        // 5: Disable Flowcontrol
        self.vsc7448
            .modify(Vsc7448::QSYS().PAUSE_CFG().PAUSE_CFG(port), |r| {
                r.set_pause_ena(0)
            })?;

        // 5.1: Disable PFC
        self.vsc7448
            .modify(Vsc7448::QRES().RES_QOS_ADV().PFC_CFG(port), |r| {
                r.set_tx_pfc_ena(0)
            })?;

        // 6: Wait a worst case time 8ms (jumbo/10Mbit)
        hl::sleep_for(8);

        // 7: Flush the queues accociated with the port
        self.vsc7448
            .modify(Vsc7448::HSCH().HSCH_MISC().FLUSH_CTRL(), |r| {
                r.set_flush_port(port);
                r.set_flush_dst(1);
                r.set_flush_src(1);
                r.set_flush_ena(1);
            })?;

        // 8: Enable dequeuing from the egress queues
        self.vsc7448
            .modify(Vsc7448::HSCH().HSCH_MISC().PORT_MODE(port), |r| {
                r.set_dequeue_dis(0)
            })?;

        // 9: Wait until flushing is complete
        self.port_flush_wait(port)?;

        Ok(())
    }

    /// Waits for a port flush to finish.  This is based on
    /// `jr2_port_flush_poll` in the MESA SDK
    fn port_flush_wait(&self, port: u32) -> Result<(), VscError> {
        for _ in 0..32 {
            let mut empty = true;
            // DST-MEM and SRC-MEM
            for base in [0, 2048] {
                for prio in 0..8 {
                    let value = self.vsc7448.read(
                        Vsc7448::QRES()
                            .RES_CTRL(base + 8 * port + prio)
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
        return Err(VscError::PortFlushTimeout { port: port as u8 });
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
