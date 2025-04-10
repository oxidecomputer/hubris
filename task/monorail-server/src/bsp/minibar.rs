// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_monorail_api::MonorailError;
use idol_runtime::RequestError;
use ringbuf::*;
use userlib::{hl::sleep_for, UnwrapLite};
use vsc7448::{
    config::Speed, miim_phy::Vsc7448MiimPhy, Vsc7448, Vsc7448Rw, VscError,
};
use vsc7448_pac::{DEVCPU_GCB, HSIO, VAUI0, VAUI1};
use vsc85xx::{vsc8504::Vsc8504, PhyRw};

/// Interval at which `Bsp::wake()` is called by the main loop
pub const WAKE_INTERVAL: Option<u32> = Some(500);

#[derive(Copy, Clone, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    Reinit,
    RearIoSpeedChange {
        port: u8,
        before: Speed,
        #[count(children)]
        after: Speed,
    },
    AnegCheckFailed(#[count(children)] VscError),
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

pub struct Bsp<'a, R> {
    vsc7448: &'a Vsc7448<'a, R>,

    /// PHY for the on-board PHY
    vsc8504: Vsc8504,

    /// Configured speed of ports on the front IO board, from the perspective of
    /// the VSC7448.
    ///
    /// They are initially configured to 1G, but the VSC8504 PHY may
    /// autonegotiate to a different speed, in which case we have to reconfigure
    /// the port on the VSC7448 to match.
    rear_io_speed: [Speed; 3],
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

    // See Figure 8, QSGMII Muxing in the datasheet (VMDS-10498)
    pub const PORT_MAP: PortMap = PortMap::new([
        SGMII,     // 0  | DEV1G_0   | SERDES1G_1  |  Sled SP link 0
        SGMII,     // 1  | DEV1G_1   | SERDES1G_2  |  Sled SP link 1
        SGMII,     // 2  | DEV1G_2   | SERDES1G_3  | Local SP link 0
        SGMII,     // 3  | DEV1G_3   | SERDES1G_4  | Local SP link 1
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
        QSGMII_1G, // 40 | DEV1G_16  | SERDES6G_14 | Rear ethernet jack 0
        QSGMII_1G, // 41 | DEV1G_17  | SERDES6G_14 | Rear ethernet jack 1
        QSGMII_1G, // 42 | DEV1G_18  | SERDES6G_14 | Rear ethernet jack 2
        QSGMII_1G, // 43 | Unused
        None,      // 44
        None,      // 45
        None,      // 46
        None,      // 47
        None,      // 48
        None,      // 49
        None,      // 50
        None,      // 51
        None,      // 52
    ]);
}
pub use map::PORT_MAP;

pub fn preinit() {
    sleep_for(100); // wait for power to stabilize (XXX is this needed?)
}

impl<'a, R: Vsc7448Rw> Bsp<'a, R> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448<'a, R>) -> Result<Self, VscError> {
        let mut out = Bsp {
            vsc7448,
            vsc8504: Vsc8504::empty(),
            rear_io_speed: [Speed::Speed1G; 3],
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
        // the call to `configure_vlan_minibar` below)
        //
        // The root cause is unknown, but we suspect a hardware race condition
        // in the switch IC; see this issue for detailed discussion:
        // https://github.com/oxidecomputer/hubris/issues/1399
        self.vsc7448.configure_vlan_none()?;

        // Reset internals
        self.vsc8504 = Vsc8504::empty();
        self.rear_io_speed = [Speed::Speed1G; 3];

        self.phy_vsc8504_init()?;

        self.vsc7448.configure_ports_from_map(&PORT_MAP)?;
        self.vsc7448.configure_vlan_minibar()?;
        self.vsc7448_postconfig()?;

        Ok(())
    }

    fn vsc7448_postconfig(&mut self) -> Result<(), VscError> {
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

        Ok(())
    }

    /// Configures the local PHY, which is an on-board VSC8504
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
        self.vsc8504 = Vsc8504::init_qsgmii_to_cat5(4, rw)?;
        for p in 5..8 {
            Vsc8504::init_qsgmii_to_cat5(p, rw)?; // XXX is this necessary?
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

    fn check_aneg_speed(
        &mut self,
        switch_port: u8,
        phy_port: u8,
    ) -> Result<(), VscError> {
        use vsc7448_pac::phy::*;

        let phy_rw = &mut Vsc7448MiimPhy::new(self.vsc7448, 0);
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
            let current_speed = self.rear_io_speed[phy_port as usize - 4];
            if target_speed != current_speed {
                ringbuf_entry!(Trace::RearIoSpeedChange {
                    port: switch_port,
                    before: current_speed,
                    after: target_speed,
                });
                let cfg = PORT_MAP.port_config(switch_port).unwrap();
                self.vsc7448.reinit_sgmii(cfg.dev, target_speed)?;
                self.rear_io_speed[phy_port as usize - 4] = target_speed;

                // Clear a spurious MAC_CGBAD flag that pops up when we
                // change the link speed here.
                for p in 0..2 {
                    use vsc7448_pac::phy;
                    vsc85xx::Phy::new(p, phy_rw)
                        .read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS())?;
                }
            }
        }
        Ok(())
    }

    pub fn wake(&mut self) -> Result<(), VscError> {
        // Check for autonegotiation on the rear IO ports, then reconfigure
        // on the switch side to change speeds.
        for port in 40..=42 {
            match self.check_aneg_speed(port, port - 40 + 4) {
                Ok(()) => (),
                Err(e) => ringbuf_entry!(Trace::AnegCheckFailed(e)),
            }
        }

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
            _ => return None,
        };
        let phy = vsc85xx::Phy::new(phy_port, &mut phy_rw);
        Some(callback(phy))
    }

    pub(crate) fn unlock_vlans_until(
        &mut self,
        _unlock_until: u64,
    ) -> Result<(), RequestError<MonorailError>> {
        Err(MonorailError::NotSupported.into())
    }

    pub(crate) fn lock_vlans(
        &mut self,
    ) -> Result<(), RequestError<MonorailError>> {
        Err(MonorailError::NotSupported.into())
    }
}

/// Simple enum that contains all possible `PhyRw` handle types
pub enum GenericPhyRw<'a, R> {
    Vsc7448(Vsc7448MiimPhy<'a, R>),
}

impl<'a, R: Vsc7448Rw> PhyRw for GenericPhyRw<'a, R> {
    #[inline(always)]
    fn read_raw(&self, port: u8, reg: u8) -> Result<u16, VscError> {
        match self {
            GenericPhyRw::Vsc7448(n) => n.read_raw(port, reg),
        }
    }
    #[inline(always)]
    fn write_raw(&self, port: u8, reg: u8, value: u16) -> Result<(), VscError> {
        match self {
            GenericPhyRw::Vsc7448(n) => n.write_raw(port, reg, value),
        }
    }
}
