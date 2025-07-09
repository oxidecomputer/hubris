// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_front_io_api::FrontIO;
use drv_medusa_seq_api::Sequencer;
use drv_monorail_api::MonorailError;
use idol_runtime::{ClientError, RequestError};
use ringbuf::*;
use task_net_api::Net;
use userlib::task_slot;
use vsc7448::{config::Speed, Vsc7448, Vsc7448Rw, VscError};
use vsc7448_pac::{HSIO, VAUI0, VAUI1};
use vsc85xx::{vsc8562::Vsc8562Phy, vsc85x2::Vsc85x2, PhyRw};

task_slot!(NET, net);
task_slot!(SEQ, seq);
task_slot!(FRONT_IO, front_io);

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

    /// handle for the front-io task
    front_io: FrontIO,

    // handle for Medusa's sequencer task
    seq: Sequencer,

    /// RPC handle for the front IO board's PHY, which is a VSC8562. This is
    /// used for PHY control via a Rube Goldberg machine of
    ///     Hubris RPC -> SPI -> FPGA -> MDIO -> PHY
    ///
    /// This is `None` if the front IO board isn't connected.
    vsc8562_front: Option<PhySmi>,

    /// RPC handle for Medusa's local techport PHY
    vsc8562_local: NetSmi,

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
    const QSGMII_1G: Option<PortMode> = Some(Qsgmii(Speed1G));

    // See RFD144 for a detailed look at the design
    pub const PORT_MAP: PortMap = PortMap::new([
        None,      // 0
        None,      // 1
        None,      // 2
        None,      // 3
        None,      // 4
        None,      // 5
        None,      // 6
        None,      // 7
        None,      // 8
        None,      // 9
        None,      // 10
        None,      // 11
        None,      // 12
        None,      // 13
        None,      // 14
        None,      // 15
        None,      // 16
        None,      // 17
        None,      // 18
        None,      // 19
        None,      // 20
        None,      // 21
        None,      // 22
        None,      // 23
        None,      // 24
        None,      // 25
        None,      // 26
        None,      // 27
        None,      // 28
        None,      // 29
        None,      // 30
        None,      // 31
        None,      // 32
        None,      // 33
        None,      // 34
        None,      // 35
        None,      // 36
        None,      // 37
        None,      // 38
        None,      // 39
        QSGMII_1G, // 40 | DEV1G_16  | SERDES6G_14 | Local Technician 1
        QSGMII_1G, // 41 | DEV1G_17  | SERDES6G_14 | Local Technician 2
        None,      // 42 | Unused (configured in QSGMII mode by port 40)
        None,      // 43 | Unused (configured in QSGMII mode by port 40)
        QSGMII_1G, // 44 | DEV1G_20  | SERDES6G_15 | Technician 1
        QSGMII_1G, // 45 | DEV1G_21  | SERDES6G_15 | Technician 2
        None,      // 46 | Unused (configured in QSGMII mode by port 44)
        None,      // 47 | Unused (configured in QSGMII mode by port 44)
        SGMII,     // 48 | DEV2G5_24 | SERDES1G_0 | Local SP
        None,      // 49
        None,      // 50
        None,      // 51
        None,      // 52
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
        // Medusa's sequencer task handles some sequencing done by an FPGA on other designs
        while !seq.vsc7448_ready() && !seq.local_vsc8562_ready() {
            userlib::hl::sleep_for(10);
        }

        let front_io = FrontIO::from(FRONT_IO.get_task_id());
        let has_front_io = front_io.board_present();
        let mut out = Bsp {
            vsc7448,
            vsc8562_front: if has_front_io {
                Some(PhySmi::new(FRONT_IO.get_task_id()))
            } else {
                None
            },
            front_io_speed: [Speed::Speed1G; 2],
            link_down_at: None,
            vsc8562_local: NetSmi::new(NET.get_task_id()),
            front_io,
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
        self.front_io_speed = [Speed::Speed1G; 2];

        self.vsc7448.configure_ports_from_map(&PORT_MAP)?;
        self.vsc7448.configure_vlan_sidecar_unlocked()?;
        self.vsc7448_postconfig()?;

        // Some front IO boards have a faulty oscillator driving the PHY,
        // causing its clock to misbehave some fraction of (re-)boots. Init
        // the PHY in a loop, requesting the sequencer to reset as much as
        // necessary to try and correct the problem.
        let mut osc_good = false;

        while self.vsc8562_front.is_some() && !osc_good {
            self.phy_vsc8562_init()?;

            osc_good = self.is_front_io_link_good()?;

            // Notify the front IO server about the state of the oscillator. If the
            // oscillator is good any future resets of the PHY do not require a
            // full power cycle of the front IO board.
            self.front_io
                .phy_set_osc_state(osc_good)
                .map_err(|e| VscError::ProxyError(e.into()))?;

            if !osc_good {
                ringbuf_entry!(Trace::FrontIoPhyOscillatorBad)
            }
        }

        if let Some(phy_rw) = &mut self.vsc8562_front {
            // Read the MAC_SERDES_PCS_STATUS register to clear a spurious
            // MAC_CGBAD error that shows up on startup.
            for p in 0..2 {
                use vsc7448_pac::phy;
                vsc85xx::Phy::new(p, phy_rw)
                    .read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS())?;
            }
        }

        // Finally, handle configuring the local PHY
        for p in 2..=3 {
            let mut phy = vsc85xx::Phy::new(p, &mut self.vsc8562_local);
            let mut v = Vsc8562Phy { phy: &mut phy };
            v.init_qsgmii()?;
        }

        self.seq.set_local_phy_coma_mode(false);

        for p in 2..=3 {
            use vsc7448_pac::phy;
            vsc85xx::Phy::new(p, &mut self.vsc8562_local)
                .read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS())?;
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

        // Tune QSGMII link from the front IO board's PHY
        // These values are captured empirically with an oscilloscope
        if let Some(phy) = self.vsc8562_front.as_mut() {
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

    pub fn phy_vsc8562_init(&mut self) -> Result<(), VscError> {
        if let Some(phy_rw) = &mut self.vsc8562_front {
            // Request a reset of the PHY. If we had previously marked the PHY
            // oscillator as bad, then this power-cycles the entire front IO
            // board; otherwise, it only power-cycles the PHY.
            self.front_io
                .phy_reset()
                .map_err(|e| VscError::ProxyError(e.into()))?;

            for p in 0..2 {
                let mut phy = vsc85xx::Phy::new(p, phy_rw);
                let mut v = Vsc8562Phy { phy: &mut phy };
                v.init_qsgmii()?;
            }
            self.front_io
                .phy_set_coma_mode(false)
                .map_err(|e| VscError::ProxyError(e.into()))?;
        }

        Ok(())
    }

    fn check_aneg_speed(
        &mut self,
        switch_port: u8,
        phy_port: u8,
    ) -> Result<(), VscError> {
        if let Some(phy_rw) = &mut self.vsc8562_front {
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
    pub fn phy_fn<T, F: Fn(vsc85xx::Phy<'_, GenericPhyRw<'_>>) -> T>(
        &mut self,
        port: u8,
        callback: F,
    ) -> Option<T> {
        let (mut phy_rw, phy_port) = match port {
            // Port 43 connects to a VSC8562 via the SP's SMI interface
            40..=41 => {
                let phy_rw = GenericPhyRw::Local(&self.vsc8562_local);
                (phy_rw, port - 40 + 2)
            }
            // Ports 44/45 connect to a VSC8562 via the Front I/O interface
            44..=45 => {
                if let Some(phy_rw) = &self.vsc8562_front {
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

pub struct PhySmi {
    front_io_board: FrontIO,
}

impl PhySmi {
    pub fn new(front_io_task: userlib::TaskId) -> Self {
        Self {
            front_io_board: FrontIO::from(front_io_task),
        }
    }
}

impl PhyRw for PhySmi {
    #[inline(always)]
    fn read_raw(&self, phy: u8, reg: u8) -> Result<u16, VscError> {
        self.front_io_board
            .phy_read(phy, reg)
            .map_err(|e| VscError::ProxyError(e.into()))
    }

    #[inline(always)]
    fn write_raw(&self, phy: u8, reg: u8, value: u16) -> Result<(), VscError> {
        self.front_io_board
            .phy_write(phy, reg, value)
            .map_err(|e| VscError::ProxyError(e.into()))
    }
}

pub struct NetSmi {
    net: Net,
}

impl NetSmi {
    pub fn new(net_task: userlib::TaskId) -> Self {
        Self {
            net: Net::from(net_task),
        }
    }
}

impl PhyRw for NetSmi {
    #[inline(always)]
    fn read_raw(&self, phy: u8, reg: u8) -> Result<u16, VscError> {
        // Net::smi_read is infallible, so an error means the task died
        self.net
            .smi_read(phy, reg)
            .map_err(|_e| VscError::ServerDied)
    }

    #[inline(always)]
    fn write_raw(&self, phy: u8, reg: u8, value: u16) -> Result<(), VscError> {
        // Net::smi_write is infallible, so an error means the task died
        self.net
            .smi_write(phy, reg, value)
            .map_err(|_e| VscError::ServerDied)
    }
}

/// Simple enum that contains all possible `PhyRw` handle types
pub enum GenericPhyRw<'a> {
    FrontIo(&'a PhySmi),
    Local(&'a NetSmi),
}

impl<'a> PhyRw for GenericPhyRw<'a> {
    #[inline(always)]
    fn read_raw(&self, port: u8, reg: u8) -> Result<u16, VscError> {
        match self {
            GenericPhyRw::FrontIo(n) => n.read_raw(port, reg),
            GenericPhyRw::Local(n) => n.read_raw(port, reg),
        }
    }
    #[inline(always)]
    fn write_raw(&self, port: u8, reg: u8, value: u16) -> Result<(), VscError> {
        match self {
            GenericPhyRw::FrontIo(n) => n.write_raw(port, reg, value),
            GenericPhyRw::Local(n) => n.write_raw(port, reg, value),
        }
    }
}
