// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_medusa_seq_api::Sequencer;
use drv_monorail_api::MonorailError;
use drv_sidecar_front_io::phy_smi::PhySmi;
use idol_runtime::{ClientError, RequestError};
use ringbuf::*;
use userlib::{task_slot, UnwrapLite};
use vsc7448::{
    config::Speed, miim_phy::Vsc7448MiimPhy, Vsc7448, Vsc7448Rw, VscError,
};
use vsc7448_pac::{DEVCPU_GCB, HSIO, VAUI0, VAUI1};
use vsc85xx::{vsc8504::Vsc8504, vsc8562::Vsc8562Phy, PhyRw};

task_slot!(SEQ, seq);
task_slot!(FRONT_IO, ecp5_front_io);

/// Interval in milliseconds at which `Bsp::wake()` is called by the main loop
pub const WAKE_INTERVAL: Option<u32> = Some(500);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    FrontIoSpeedChange {
        port: u8,
        before: Speed,
        after: Speed,
    },
    FrontIoPhyOscillatorBad,
    AnegCheckFailed(VscError),
    Reinit,
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

pub struct Bsp<'a, R> {
    vsc7448: &'a Vsc7448<'a, R>,

    /// Handle for the sequencer task
    seq: Sequencer,

    /// PHY for the on-board PHY ("PHY4")
    vsc8504: Vsc8504,

    /// RPC handle for the front IO board's PHY, which is a VSC8562. This is
    /// used for PHY control via a Rube Goldberg machine of
    ///     Hubris RPC -> SPI -> FPGA -> MDIO -> PHY
    ///
    /// This is `None` if the front IO board isn't connected.
    vsc8562: Option<PhySmi>,

    /// Configured speed of ports on the front IO board, from the perspective of
    /// the VSC7448.
    ///
    /// They are initially configured to 1G, but the VSC8562 PHY may
    /// autonegotiate to a different speed, in which case we have to reconfigure
    /// the port on the VSC7448 to match.
    front_io_speed: [Speed; 2],

    /// Time at which the 10G link went down
    link_down_at: Option<u64>,
}

pub const REFCLK_SEL: vsc7448::RefClockFreq =
    vsc7448::RefClockFreq::Clk156p25MHz;
pub const REFCLK2_SEL: Option<vsc7448::RefClockFreq> = None;

mod map {
    // Local module to avoid leaking imports
    use vsc7448::config::{
        PortMap,
        PortMode::{self, *},
        Speed::*,
    };
    const SGMII: Option<PortMode> = Some(Sgmii(Speed100M));
    const QSGMII_100M: Option<PortMode> = Some(Qsgmii(Speed100M));
    const QSGMII_1G: Option<PortMode> = Some(Qsgmii(Speed1G));
    const BASE_KR: Option<PortMode> = Some(BaseKr);

    // See RFD144 for a detailed look at the design
    pub const PORT_MAP: PortMap = PortMap::new([
        SGMII,       // 0  | DEV1G_0   | SERDES1G_1  | Cubby 0
        SGMII,       // 1  | DEV1G_1   | SERDES1G_2  | Cubby 1
        SGMII,       // 2  | DEV1G_2   | SERDES1G_3  | Cubby 2
        SGMII,       // 3  | DEV1G_3   | SERDES1G_4  | Cubby 3
        SGMII,       // 4  | DEV1G_4   | SERDES1G_5  | Cubby 4
        SGMII,       // 5  | DEV1G_5   | SERDES1G_6  | Cubby 5
        SGMII,       // 6  | DEV1G_6   | SERDES1G_7  | Cubby 6
        SGMII,       // 7  | DEV1G_7   | SERDES1G_8  | Cubby 7
        SGMII,       // 8  | DEV2G5_0  | SERDES6G_0  | Cubby 8
        SGMII,       // 9  | DEV2G5_1  | SERDES6G_1  | Cubby 9
        SGMII,       // 10 | DEV2G5_2  | SERDES6G_2  | Cubby 10
        SGMII,       // 11 | DEV2G5_3  | SERDES6G_3  | Cubby 11
        SGMII,       // 12 | DEV2G5_4  | SERDES6G_4  | Cubby 12
        SGMII,       // 13 | DEV2G5_5  | SERDES6G_5  | Cubby 13
        SGMII,       // 14 | DEV2G5_6  | SERDES6G_6  | Cubby 14
        SGMII,       // 15 | DEV2G5_7  | SERDES6G_7  | Cubby 15
        SGMII,       // 16 | DEV2G5_8  | SERDES6G_8  | Cubby 16
        SGMII,       // 17 | DEV2G5_9  | SERDES6G_9  | Cubby 17
        SGMII,       // 18 | DEV2G5_10 | SERDES6G_10 | Cubby 18
        SGMII,       // 19 | DEV2G5_11 | SERDES6G_11 | Cubby 19
        SGMII,       // 20 | DEV2G5_12 | SERDES6G_12 | Cubby 20
        SGMII,       // 21 | DEV2G5_13 | SERDES6G_13 | Cubby 21
        None,        // 22
        None,        // 23
        SGMII,       // 24 | DEV2G5_16 | SERDES6G_16 | Cubby 22
        SGMII,       // 25 | DEV2G5_17 | SERDES6G_17 | Cubby 23
        SGMII,       // 26 | DEV2G5_18 | SERDES6G_18 | Cubby 24
        SGMII,       // 27 | DEV2G5_19 | SERDES6G_19 | Cubby 25
        SGMII,       // 28 | DEV2G5_20 | SERDES6G_20 | Cubby 26
        SGMII,       // 29 | DEV2G5_21 | SERDES6G_21 | Cubby 27
        SGMII,       // 30 | DEV2G5_22 | SERDES6G_22 | Cubby 28
        SGMII,       // 31 | DEV2G5_23 | SERDES6G_23 | Cubby 29
        None,        // 32
        None,        // 33
        None,        // 34
        None,        // 35
        None,        // 36
        None,        // 37
        None,        // 38
        None,        // 39
        QSGMII_100M, // 40 | DEV1G_16  | SERDES6G_14 | Peer SP
        QSGMII_100M, // 41 | DEV1G_17  | SERDES6G_14 | PSC0
        QSGMII_100M, // 42 | DEV1G_18  | SERDES6G_14 | PSC1
        QSGMII_100M, // 43 | Unused
        QSGMII_1G,   // 44 | DEV1G_20  | SERDES6G_15 | Technician 1
        QSGMII_1G,   // 45 | DEV1G_21  | SERDES6G_15 | Technician 2
        None,        // 46 | Unused (configured in QSGMII mode by port 44)
        None,        // 47 | Unused (configured in QSGMII mode by port 44)
        SGMII,       // 48 | DEV2G5_24 | SERDES1G_0 | Local SP
        BASE_KR,     // 49 | DEV10G_0  | SERDES10G_0 | Tofino 2
        None,        // 50 | Unused
        SGMII, // 51 | DEV2G5_27 | SERDES10G_2 | Cubby 30 (shadows DEV10G_2)
        SGMII, // 52 | DEV2G5_28 | SERDES10G_3 | Cubby 31 (shadows DEV10G_3)
    ]);
}
pub use map::PORT_MAP;

pub fn preinit() {
    // Nothing to do here, just stubbing out for the BSP interface
}

impl<'a, R: Vsc7448Rw> Bsp<'a, R> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448<'a, R>) -> Result<Self, VscError> {
        let seq = Sequencer::from(SEQ.get_task_id());
        let has_front_io = seq.front_io_board_present();
        let mut out = Bsp {
            vsc7448,
            vsc8504: Vsc8504::empty(),
            vsc8562: if has_front_io {
                Some(PhySmi::new(FRONT_IO.get_task_id()))
            } else {
                None
            },
            front_io_speed: [Speed::Speed1G; 2],
            link_down_at: None,
            seq,
        };

        out.reinit()?;
        Ok(out)
    }

    pub fn reinit(&mut self) -> Result<(), VscError> {
        ringbuf_entry!(Trace::Reinit);
        self.vsc7448.init()?;

        // By default, the SERDES6G are grouped into 4x chunks for XAUI,
        // where a single DEV10G runs 4x SERDES6G at 2.5G.  This leads to very
        // confusing behavior when only running a few SERDES6G: in particularly,
        // we noticed that SERDES6G_14 seemed to depend on SERDES6G_12.
        //
        // We're never using this "lane sync" feature, so disable it everywhere.
        for i in 0..=1 {
            self.vsc7448.modify(
                VAUI0().VAUI_CHANNEL_CFG().VAUI_CHANNEL_CFG(i),
                |r| {
                    r.set_lane_sync_ena(0);
                },
            )?;
            self.vsc7448.modify(
                VAUI1().VAUI_CHANNEL_CFG().VAUI_CHANNEL_CFG(i),
                |r| {
                    r.set_lane_sync_ena(0);
                },
            )?;
        }

        // We must disable frame copying before configuring ports; otherwise, a
        // rare failure mode can result in queues getting stuck (forever!).  We
        // disable frame copying by enabling VLANs, then removing all ports from
        // them!
        //
        // (ports will be added back to VLANs after configuration is done, in
        // the call to `configure_vlan_sidecar_unlocked` below)
        //
        // The root cause is unknown, but we suspect a hardware race condition
        // in the switch IC; see this issue for detailed discussion:
        // https://github.com/oxidecomputer/hubris/issues/1399
        self.vsc7448.configure_vlan_none()?;

        // Reset internals
        self.vsc8504 = Vsc8504::empty();
        self.front_io_speed = [Speed::Speed1G; 2];

        self.phy_vsc8504_init()?;

        self.vsc7448.configure_ports_from_map(&PORT_MAP)?;
        self.vsc7448.configure_vlan_sidecar_unlocked()?;
        self.vsc7448_postconfig()?;

        // Some front IO boards have a faulty oscillator driving the PHY,
        // causing its clock to misbehave some fraction of (re-)boots. Init
        // the PHY in a loop, requesting the sequencer to reset as much as
        // necessary to try and correct the problem.
        let mut osc_good = false;

        while self.vsc8562.is_some() && !osc_good {
            self.phy_vsc8562_init()?;

            osc_good = self.is_front_io_link_good()?;

            // Notify the sequencer about the state of the oscillator. If the
            // oscillator is good any future resets of the PHY do not require a
            // full power cycle of the front IO board.
            self.seq
                .set_front_io_phy_osc_state(osc_good)
                .map_err(|e| VscError::ProxyError(e.into()))?;

            if !osc_good {
                ringbuf_entry!(Trace::FrontIoPhyOscillatorBad)
            }
        }

        if let Some(phy_rw) = &mut self.vsc8562 {
            // Read the MAC_SERDES_PCS_STATUS register to clear a spurious
            // MAC_CGBAD error that shows up on startup.
            for p in 0..2 {
                use vsc7448_pac::phy;
                vsc85xx::Phy::new(p, phy_rw)
                    .read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS())?;
            }
        }

        Ok(())
    }

    fn vsc7448_postconfig(&mut self) -> Result<(), VscError> {
        // The SERDES6G going to the front IO board needs to be tuned from
        // its default settings, otherwise the signal quality is bad.
        const FRONT_IO_SERDES6G: u8 = 15;
        vsc7448::serdes6g::serdes6g_read(self.vsc7448, FRONT_IO_SERDES6G)?;

        // h monorail write HSIO:SERDES6G_ANA_CFG:SERDES6G_OB_CFG 0x28441001
        // h monorail write HSIO:SERDES6G_ANA_CFG:SERDES6G_OB_CFG1 0x3F
        self.vsc7448.modify(
            HSIO().SERDES6G_ANA_CFG().SERDES6G_OB_CFG(),
            |r| {
                r.set_ob_post0(0x10);
                r.set_ob_prec(0x11); // -1, since MSB is sign
                r.set_ob_post1(0x2);
                r.set_ob_sr_h(0); // Full-rate mode
                r.set_ob_sr(0); // Very fast edges (30 ps)
            },
        )?;
        self.vsc7448.modify(
            HSIO().SERDES6G_ANA_CFG().SERDES6G_OB_CFG1(),
            |r| {
                r.set_ob_lev(0x3F);
            },
        )?;
        vsc7448::serdes6g::serdes6g_write(self.vsc7448, FRONT_IO_SERDES6G)?;

        // Same for the on-board QSGMII link to the VSC8504, with different
        // settings.
        // h monorail write SERDES6G_OB_CFG 0x26000131
        // h monorail write SERDES6G_OB_CFG1 0x20
        const VSC8504_SERDES6G: u8 = 14;
        vsc7448::serdes6g::serdes6g_read(self.vsc7448, VSC8504_SERDES6G)?;
        self.vsc7448.modify(
            HSIO().SERDES6G_ANA_CFG().SERDES6G_OB_CFG(),
            |r| {
                // Leave all other values as default
                r.set_ob_post0(0xc);
                r.set_ob_sr_h(1); // half-rate mode
                r.set_ob_sr(3); // medium speed edges (about 105 ps)
            },
        )?;
        self.vsc7448.modify(
            HSIO().SERDES6G_ANA_CFG().SERDES6G_OB_CFG1(),
            |r| {
                r.set_ob_lev(0x20);
            },
        )?;
        vsc7448::serdes6g::serdes6g_write(self.vsc7448, VSC8504_SERDES6G)?;

        // Write to the base port on the VSC8504, patching the SERDES6G
        // config to improve signal integrity.  This is based on benchtop
        // scoping of the QSGMII signals going from the VSC8504 to the VSC7448.
        use vsc85xx::tesla::{TeslaPhy, TeslaSerdes6gObConfig};
        let rw = &mut Vsc7448MiimPhy::new(self.vsc7448, 0);
        let mut vsc8504 = self.vsc8504.phy(0, rw);
        let mut tesla = TeslaPhy {
            phy: &mut vsc8504.phy,
        };
        tesla.tune_serdes6g_ob(TeslaSerdes6gObConfig {
            ob_post0: 0x6,
            ob_post1: 0,
            ob_prec: 0,
            ob_sr_h: 1, // half rate
            ob_sr: 0,
        })?;

        // Tune QSGMII link from the front IO board's PHY
        // These values are captured empirically with an oscilloscope
        if let Some(phy) = self.vsc8562.as_mut() {
            use vsc85xx::vsc8562::{Sd6gObCfg, Sd6gObCfg1};
            let mut p = vsc85xx::Phy::new(0, phy); // port 0
            let mut v = Vsc8562Phy { phy: &mut p };
            v.tune_sd6g_ob_cfg(Sd6gObCfg {
                ob_ena1v_mode: 1,
                ob_pol: 1,
                ob_post0: 20,
                ob_post1: 0,
                ob_sr_h: 0,
                ob_resistor_ctr: 1,
                ob_sr: 15,
            })?;
            v.tune_sd6g_ob_cfg1(Sd6gObCfg1 {
                ob_ena_cas: 0,
                ob_lev: 48,
            })?;
        }

        Ok(())
    }

    /// Configures the local PHY ("PHY4"), which is an on-board VSC8504
    fn phy_vsc8504_init(&mut self) -> Result<(), VscError> {
        // Let's configure the on-board PHY first
        //
        // It's always powered on, and COMA_MODE is controlled via the VSC7448
        // on GPIO_47.
        const COMA_MODE_GPIO: u32 = 47;

        // The PHY talks on MIIM addresses 0x4-0x7 (configured by resistors
        // on the board), using the VSC7448 as a MIIM bridge.

        // When the VSC7448 comes out of reset, GPIO_47 is an input and low.
        // It's pulled up by a resistor on the board, keeping the PHY in
        // COMA_MODE.  That's fine!

        // Initialize the PHY
        let rw = &mut Vsc7448MiimPhy::new(self.vsc7448, 0);
        self.vsc8504 = Vsc8504::init_qsgmii_protocol_xfer(4, rw)?;
        for p in 5..8 {
            Vsc8504::init_qsgmii_protocol_xfer(p, rw)?;
        }

        // The VSC8504 on the sidecar has its SIGDET GPIOs pulled down,
        // for some reason.
        self.vsc8504.set_sigdet_polarity(rw, true).unwrap_lite();

        // Switch the GPIO to an output.  Since the output register is low
        // by default, this pulls COMA_MODE low, bringing the VSC8504 into
        // mission mode.
        self.vsc7448.modify(DEVCPU_GCB().GPIO().GPIO_OE1(), |r| {
            let mut g_oe1 = r.g_oe1();
            g_oe1 |= 1 << (COMA_MODE_GPIO - 32);
            r.set_g_oe1(g_oe1);
        })?;

        Ok(())
    }

    pub fn phy_vsc8562_init(&mut self) -> Result<(), VscError> {
        if let Some(phy_rw) = &mut self.vsc8562 {
            // Request a reset of the PHY. If we had previously marked the PHY
            // oscillator as bad, then this power-cycles the entire front IO
            // board; otherwise, it only power-cycles the PHY.
            self.seq
                .reset_front_io_phy()
                .map_err(|e| VscError::ProxyError(e.into()))?;

            for p in 0..2 {
                let mut phy = vsc85xx::Phy::new(p, phy_rw);
                let mut v = Vsc8562Phy { phy: &mut phy };
                v.init_qsgmii()?;
            }
            phy_rw
                .set_coma_mode(false)
                .map_err(|e| VscError::ProxyError(e.into()))?;
        }

        Ok(())
    }

    fn check_aneg_speed(
        &mut self,
        switch_port: u8,
        phy_port: u8,
    ) -> Result<(), VscError> {
        if let Some(phy_rw) = &mut self.vsc8562 {
            use vsc7448_pac::phy::*;

            let phy = vsc85xx::Phy::new(phy_port, phy_rw);
            let status = phy.read(STANDARD::MODE_STATUS())?;

            // If autonegotiation is complete, then decide on a speed
            let target_speed = if status.0 & (1 << 5) != 0 {
                let status = phy.read(STANDARD::REG_1000BASE_T_STATUS())?;
                // Check "LP 1000BASE-T FDX capable" bit
                if status.0 & (1 << 11) != 0 {
                    Some(Speed::Speed1G)
                } else {
                    Some(Speed::Speed100M)
                }
                // TODO: 10M?
            } else {
                None
            };
            if let Some(target_speed) = target_speed {
                let current_speed = self.front_io_speed[phy_port as usize];
                if target_speed != current_speed {
                    ringbuf_entry!(Trace::FrontIoSpeedChange {
                        port: switch_port,
                        before: current_speed,
                        after: target_speed,
                    });
                    let cfg = PORT_MAP.port_config(switch_port).unwrap();
                    self.vsc7448.reinit_sgmii(cfg.dev, target_speed)?;
                    self.front_io_speed[phy_port as usize] = target_speed;

                    // Clear a spurious MAC_CGBAD flag that pops up when we
                    // change the link speed here.
                    for p in 0..2 {
                        use vsc7448_pac::phy;
                        vsc85xx::Phy::new(p, phy_rw)
                            .read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS())?;
                    }
                }
            }
        }
        Ok(())
    }

    fn is_front_io_link_good(&self) -> Result<bool, VscError> {
        // Determine if the link is up which implies the PHY oscillator is good.
        Ok(self
            .vsc7448
            .read(HSIO().HW_CFGSTAT().HW_QSGMII_STAT(11))?
            .sync()
            == 1)
    }

    pub fn wake(&mut self) -> Result<(), VscError> {
        // Check for autonegotiation on the front IO board, then reconfigure
        // on the switch side to change speeds.
        for port in 44..=45 {
            match self.check_aneg_speed(port, port - 44) {
                Ok(()) => (),
                Err(e) => ringbuf_entry!(Trace::AnegCheckFailed(e)),
            }
        }

        self.link_down_at = None;

        Ok(())
    }

    /// Calls a function on a `Phy` associated with the given port.
    ///
    /// Returns `None` if the given port isn't associated with a PHY
    /// (for example, because it's an SGMII link)
    pub fn phy_fn<T, F: Fn(vsc85xx::Phy<'_, GenericPhyRw<'_, R>>) -> T>(
        &mut self,
        port: u8,
        callback: F,
    ) -> Option<T> {
        let (mut phy_rw, phy_port) = match port {
            // Ports 40-43 connect to a VSC8504 PHY over QSGMII and represent
            // ports 4-7 on the PHY.
            40..=43 => {
                let phy_rw = GenericPhyRw::Vsc7448(Vsc7448MiimPhy::new(
                    self.vsc7448.rw,
                    0,
                ));
                let phy_port = port - 40 + 4;
                (phy_rw, phy_port)
            }
            44..=45 => {
                if let Some(phy_rw) = &self.vsc8562 {
                    (GenericPhyRw::FrontIo(phy_rw), port - 44)
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        let phy = vsc85xx::Phy::new(phy_port, &mut phy_rw);
        Some(callback(phy))
    }

    pub(crate) fn unlock_vlans_until(
        &mut self,
        _unlock_until: u64,
    ) -> Result<(), RequestError<MonorailError>> {
        // Not implemented on Medusa
        Err(RequestError::Fail(ClientError::BadMessageContents))
    }

    pub(crate) fn lock_vlans(
        &mut self,
    ) -> Result<(), RequestError<MonorailError>> {
        // Not implemented on Medusa
        Err(RequestError::Fail(ClientError::BadMessageContents))
    }
}

/// Simple enum that contains all possible `PhyRw` handle types
pub enum GenericPhyRw<'a, R> {
    Vsc7448(Vsc7448MiimPhy<'a, R>),
    FrontIo(&'a PhySmi),
}

impl<'a, R: Vsc7448Rw> PhyRw for GenericPhyRw<'a, R> {
    #[inline(always)]
    fn read_raw(&self, port: u8, reg: u8) -> Result<u16, VscError> {
        match self {
            GenericPhyRw::Vsc7448(n) => n.read_raw(port, reg),
            GenericPhyRw::FrontIo(n) => n.read_raw(port, reg),
        }
    }
    #[inline(always)]
    fn write_raw(&self, port: u8, reg: u8, value: u16) -> Result<(), VscError> {
        match self {
            GenericPhyRw::Vsc7448(n) => n.write_raw(port, reg, value),
            GenericPhyRw::FrontIo(n) => n.write_raw(port, reg, value),
        }
    }
}
