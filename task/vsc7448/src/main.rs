#![no_std]
#![no_main]

use ringbuf::*;
use userlib::*;

// Flags to tune ringbuf output while developing
const DEBUG_TRACE_SPI: u8 = 1 << 0;
const DEBUG_TRACE_MIIM: u8 = 1 << 1;
const DEBUG_MASK: u8 = 0;

/// Writes the given value to the ringbuf if allowed by the global `DEBUG_MASK`
macro_rules! ringbuf_entry_masked {
    ($mask:ident, $value:expr) => {
        if (DEBUG_MASK & $mask) != 0 {
            ringbuf_entry!($value);
        }
    };
}

use drv_spi_api::{Spi, SpiDevice, SpiError};
use vsc7448_pac::{
    phy,
    types::{PhyRegisterAddress, RegisterAddress},
    Vsc7448,
};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Start(u64),
    Read {
        addr: u32,
        value: u32,
    },
    Write {
        addr: u32,
        value: u32,
    },
    MiimSetPage {
        miim: u8,
        phy: u8,
        page: u16,
    },
    MiimRead {
        miim: u8,
        phy: u8,
        page: u16,
        addr: u8,
        value: u16,
    },
    MiimWrite {
        miim: u8,
        phy: u8,
        page: u16,
        addr: u8,
        value: u16,
    },
    MiimIdleWait,
    MiimReadWait,
    PhyScanError {
        miim: u8,
        phy: u8,
        err: VscError,
    },
    PhyLinkChanged {
        port: u8,
        status: u16,
    },
    Initialized(u64),
    FailedToInitialize(VscError),
}

ringbuf!(Trace, 64, Trace::None);

task_slot!(SPI, spi_driver);
const VSC7448_SPI_DEVICE: u8 = 0;

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq)]
enum VscError {
    SpiError(SpiError),
    BadChipId(u32),
    MiimReadErr {
        miim: u8,
        phy: u8,
        page: u16,
        addr: u8,
    },
    BadPhyId1(u16),
    BadPhyId2(u16),
    MiimIdleTimeout,
    MiimReadTimeout,
    Serdes6gReadTimeout {
        instance: u16,
    },
    Serdes6gWriteTimeout {
        instance: u16,
    },
    PortFlushTimeout {
        port: u8,
    },
}

impl From<SpiError> for VscError {
    fn from(s: SpiError) -> Self {
        Self::SpiError(s)
    }
}

/// Helper struct to read and write from the VSC7448 over SPI
struct Vsc7448Spi(SpiDevice);
impl Vsc7448Spi {
    /// Reads from a VSC7448 register
    fn read<T>(&self, reg: RegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u32>,
    {
        assert!(reg.addr >= 0x71000000);
        assert!(reg.addr <= 0x72000000);
        let addr = (reg.addr & 0x00FFFFFF) >> 2;
        let data: [u8; 3] = [
            ((addr >> 16) & 0xFF) as u8,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
        ];

        // We read back 8 bytes in total:
        // - 3 bytes of address
        // - 1 byte of padding
        // - 4 bytes of data
        let mut out = [0; 8];
        self.0.exchange(&data[..], &mut out[..])?;
        let value = (out[7] as u32)
            | ((out[6] as u32) << 8)
            | ((out[5] as u32) << 16)
            | ((out[4] as u32) << 24);

        ringbuf_entry_masked!(
            DEBUG_TRACE_SPI,
            Trace::Read {
                addr: reg.addr,
                value
            }
        );
        Ok(value.into())
    }

    /// Writes to a VSC7448 register.  This will overwrite the entire register;
    /// if you want to modify it, then use [Self::modify] instead.
    fn write<T>(
        &self,
        reg: RegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u32: From<T>,
    {
        assert!(reg.addr >= 0x71000000);
        assert!(reg.addr <= 0x72000000);

        let addr = (reg.addr & 0x00FFFFFF) >> 2;
        let value: u32 = value.into();
        let data: [u8; 7] = [
            0x80 | ((addr >> 16) & 0xFF) as u8,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
            ((value >> 24) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            (value & 0xFF) as u8,
        ];

        ringbuf_entry_masked!(
            DEBUG_TRACE_SPI,
            Trace::Write {
                addr: reg.addr,
                value: value.into()
            }
        );
        self.0.write(&data[..])?;
        Ok(())
    }

    /// Writes to a port mask, which is assumed to be a pair of adjacent
    /// registers representing all 53 ports.
    fn write_port_mask<T>(
        &self,
        mut reg: RegisterAddress<T>,
        value: u64,
    ) -> Result<(), VscError>
    where
        T: From<u32>,
        u32: From<T>,
    {
        self.write(reg, ((value & 0xFFFFFFFF) as u32).into())?;
        reg.addr += 4; // Good luck!
        self.write(reg, (((value >> 32) as u32) & 0x1FFFFF).into())
    }

    /// Performs a write operation on the given register, where the value is
    /// calculated by calling f(0).  This is helpful as a way to reduce manual
    /// type information.
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

    /// Builds a MII_CMD register based on the given phy and register.  Note
    /// that miim_cmd_opr_field is unset; you must configure it for a read
    /// or write yourself.
    fn miim_cmd(
        phy: u8,
        reg_addr: u8,
    ) -> vsc7448_pac::devcpu_gcb::miim::MII_CMD {
        let mut v: vsc7448_pac::devcpu_gcb::miim::MII_CMD = 0.into();
        v.set_miim_cmd_vld(1);
        v.set_miim_cmd_phyad(phy as u32);
        v.set_miim_cmd_regad(reg_addr as u32);
        v
    }

    /// Writes a register to the PHY without modifying the page.  This
    /// shouldn't be called directly, as the page could be in an unknown
    /// state.
    fn phy_write_inner<T: From<u16>>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
    {
        let value: u16 = value.into();
        let mut v = Self::miim_cmd(phy, reg.addr);
        v.set_miim_cmd_opr_field(0b01); // read
        v.set_miim_cmd_wrdata(value as u32);

        self.miim_idle_wait(miim)?;
        self.write(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_CMD(), v)
    }

    /// Waits for the PENDING_RD and PENDING_WR bits to go low, indicating that
    /// it's safe to read or write to the MIIM.
    fn miim_idle_wait(&self, miim: u8) -> Result<(), VscError> {
        for _i in 0..32 {
            let status = self
                .read(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_STATUS())?;
            if status.miim_stat_opr_pend() == 0 {
                return Ok(());
            } else {
                ringbuf_entry!(Trace::MiimIdleWait);
            }
        }
        return Err(VscError::MiimIdleTimeout);
    }

    /// Waits for the STAT_BUSY bit to go low, indicating that a read has
    /// finished and data is available.
    fn miim_read_wait(&self, miim: u8) -> Result<(), VscError> {
        for _i in 0..32 {
            let status = self
                .read(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_STATUS())?;
            if status.miim_stat_busy() == 0 {
                return Ok(());
            } else {
                ringbuf_entry!(Trace::MiimReadWait);
            }
        }
        return Err(VscError::MiimReadTimeout);
    }

    /// Reads a register from the PHY without modifying the page.  This
    /// shouldn't be called directly, as the page could be in an unknown
    /// state.
    fn phy_read_inner<T: From<u16>>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        let mut v = Self::miim_cmd(phy, reg.addr);
        v.set_miim_cmd_opr_field(0b10); // read

        self.miim_idle_wait(miim)?;
        self.write(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_CMD(), v)?;
        self.miim_read_wait(miim)?;

        let out =
            self.read(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_DATA())?;
        if out.miim_data_success() == 0b11 {
            return Err(VscError::MiimReadErr {
                miim,
                phy,
                page: reg.page,
                addr: reg.addr,
            });
        }

        let value = out.miim_data_rddata() as u16;
        Ok(value.into())
    }

    /// Reads a register from the PHY using the MIIM interface
    fn phy_read<T>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        ringbuf_entry_masked!(
            DEBUG_TRACE_MIIM,
            Trace::MiimSetPage {
                miim,
                phy,
                page: reg.page,
            }
        );
        self.phy_write_inner::<phy::standard::PAGE>(
            miim,
            phy,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        let out = self.phy_read_inner(miim, phy, reg)?;
        ringbuf_entry_masked!(
            DEBUG_TRACE_MIIM,
            Trace::MiimRead {
                miim,
                phy,
                page: reg.page,
                addr: reg.addr,
                value: out.clone().into(),
            }
        );
        Ok(out)
    }

    /// Writes a register to the PHY using the MIIM interface
    fn phy_write<T>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        ringbuf_entry_masked!(
            DEBUG_TRACE_MIIM,
            Trace::MiimSetPage {
                miim,
                phy,
                page: reg.page,
            }
        );
        self.phy_write_inner::<phy::standard::PAGE>(
            miim,
            phy,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        ringbuf_entry_masked!(
            DEBUG_TRACE_MIIM,
            Trace::MiimWrite {
                miim,
                phy,
                page: reg.page,
                addr: reg.addr,
                value: value.clone().into(),
            }
        );
        self.phy_write_inner(miim, phy, reg, value)
    }

    /// Performs a read-modify-write operation on a PHY register connected
    /// to the VSC7448 via MIIM.
    fn phy_modify<T, F>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(&mut T),
    {
        let mut data = self.phy_read(miim, phy, reg)?;
        f(&mut data);
        self.phy_write(miim, phy, reg, data)
    }
}

////////////////////////////////////////////////////////////////////////////////
#[cfg(target_board = "gemini-bu-1")]
struct Bsp<'a> {
    vsc7448: &'a Vsc7448Spi,
}
#[cfg(target_board = "gemini-bu-1")]
impl<'a> Bsp<'a> {
    /// Constructs and initializes a new BSP handle
    fn new(vsc7448: &'a Vsc7448Spi) -> Result<Self, VscError> {
        let out = Bsp { vsc7448 };
        out.init()?;
        Ok(out)
    }

    /// Attempts to initialize the system.  This is based on a VSC7448 dev kit
    /// (VSC5627EV), so will need to change depending on your system.
    fn init(&self) -> Result<(), VscError> {
        // We assume that the only person running on a gemini-bu-1 is Matt, who is
        // talking to a VSC7448 dev kit on his desk.  In this case, we want to
        // configure the GPIOs to allow MIIM1 and 2 to be active, by setting
        // GPIO_56-59 to Overlaid Function 1
        self.vsc7448.write(
            Vsc7448::DEVCPU_GCB().GPIO().GPIO_ALT1(0),
            0xF000000.into(),
        )?;

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

    fn run(&self) -> ! {
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

////////////////////////////////////////////////////////////////////////////////

/// Dummy default struct, which panics if ever used.
#[cfg(not(target_board = "gemini-bu-1"))]
struct Bsp {}
#[cfg(not(target_board = "gemini-bu-1"))]
impl Bsp {
    fn new(_vsc7448: &Vsc7448Spi) -> Result<Self, VscError> {
        panic!("No implementation for this board")
    }
    fn run(&self) -> ! {
        panic!("No implementation for this board")
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Performs initial configuration (endianness, soft reset, read padding) of
/// the VSC7448, then checks that its chip ID is correct.
fn init(vsc7448: &Vsc7448Spi) -> Result<Bsp, VscError> {
    // Write the byte ordering / endianness configuration
    vsc7448.write(
        Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(),
        0x81818181.into(),
    )?;

    // Trigger a soft reset
    vsc7448.write(Vsc7448::DEVCPU_GCB().CHIP_REGS().SOFT_RST(), 1.into())?;

    // Re-write byte ordering / endianness
    vsc7448.write(
        Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(),
        0x81818181.into(),
    )?;
    // Configure reads to include 1 padding byte, since we're reading quickly
    vsc7448.write(Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CFGSTAT(), 1.into())?;

    let chip_id = vsc7448.read(Vsc7448::DEVCPU_GCB().CHIP_REGS().CHIP_ID())?;
    if chip_id.rev_id() != 0x3
        || chip_id.part_id() != 0x7468
        || chip_id.mfg_id() != 0x74
        || chip_id.one() != 0x1
    {
        return Err(VscError::BadChipId(chip_id.into()));
    }

    Bsp::new(vsc7448)
}

#[export_name = "main"]
fn main() -> ! {
    ringbuf_entry!(Trace::Start(sys_get_timer().now));
    let spi = Spi::from(SPI.get_task_id()).device(VSC7448_SPI_DEVICE);
    let vsc7448 = Vsc7448Spi(spi);

    loop {
        match init(&vsc7448) {
            Ok(bsp) => {
                ringbuf_entry!(Trace::Initialized(sys_get_timer().now));
                bsp.run(); // Does not terminate
            }
            Err(e) => {
                ringbuf_entry!(Trace::FailedToInitialize(e));
                hl::sleep_for(200);
            }
        }
    }
}
