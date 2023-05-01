// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_user_leds_api::UserLeds;
use ringbuf::*;
use userlib::*;
use vsc7448::{miim_phy::Vsc7448MiimPhy, Vsc7448, Vsc7448Rw, VscError};
use vsc7448_pac::{phy, *};
use vsc85xx::{vsc8522::Vsc8522, Phy};

task_slot!(USER_LEDS, user_leds);

pub const REFCLK_SEL: vsc7448::RefClockFreq = vsc7448::RefClockFreq::Clk125MHz;
pub const REFCLK2_SEL: Option<vsc7448::RefClockFreq> =
    Some(vsc7448::RefClockFreq::Clk25MHz);

/// Interval at which `Bsp::wake()` is called by the main loop
pub const WAKE_INTERVAL: Option<u64> = Some(500);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    PhyScanError { miim: u8, phy: u8, err: VscError },
    PhyLinkChanged { port: u8, status: u16 },
    SgmiiError { dev: u8, err: VscError },
}
ringbuf!(Trace, 16, Trace::None);

mod map {
    // Local module to avoid leaking imports
    use vsc7448::config::{
        PortMap,
        PortMode::{self, *},
        Speed::*,
    };
    const SGMII: Option<PortMode> = Some(Sgmii(Speed1G));
    const QSGMII: Option<PortMode> = Some(Qsgmii(Speed1G));
    const SFI: Option<PortMode> = Some(Sfi);

    pub const PORT_MAP: PortMap = PortMap::new([
        QSGMII, // 0  | DEV1G_0   | SERDES6G_4
        QSGMII, // 1  | DEV1G_1   | SERDES6G_4
        QSGMII, // 2  | DEV1G_2   | SERDES6G_4
        QSGMII, // 3  | DEV1G_3   | SERDES6G_4
        QSGMII, // 4  | DEV1G_4   | SERDES6G_5
        QSGMII, // 5  | DEV1G_5   | SERDES6G_5
        QSGMII, // 6  | DEV1G_6   | SERDES6G_5
        QSGMII, // 7  | DEV1G_7   | SERDES6G_5
        QSGMII, // 8  | DEV2G5_0  | SERDES6G_6
        QSGMII, // 9  | DEV2G5_1  | SERDES6G_6
        QSGMII, // 10 | DEV2G5_2  | SERDES6G_6
        QSGMII, // 11 | DEV2G5_3  | SERDES6G_6
        QSGMII, // 12 | DEV2G5_4  | SERDES6G_7
        QSGMII, // 13 | DEV2G5_5  | SERDES6G_7
        QSGMII, // 14 | DEV2G5_6  | SERDES6G_7
        QSGMII, // 15 | DEV2G5_7  | SERDES6G_7
        QSGMII, // 16 | DEV2G5_8  | SERDES6G_8
        QSGMII, // 17 | DEV2G5_9  | SERDES6G_8
        QSGMII, // 18 | DEV2G5_10 | SERDES6G_8
        QSGMII, // 19 | DEV2G5_11 | SERDES6G_8
        QSGMII, // 20 | DEV2G5_12 | SERDES6G_9
        QSGMII, // 21 | DEV2G5_13 | SERDES6G_9
        QSGMII, // 22 | DEV2G5_14 | SERDES6G_9
        QSGMII, // 23 | DEV2G5_15 | SERDES6G_9
        QSGMII, // 24 | DEV2G5_16 | SERDES6G_10
        QSGMII, // 25 | DEV2G5_17 | SERDES6G_10
        QSGMII, // 26 | DEV2G5_18 | SERDES6G_10
        QSGMII, // 27 | DEV2G5_19 | SERDES6G_10
        QSGMII, // 28 | DEV2G5_20 | SERDES6G_11
        QSGMII, // 29 | DEV2G5_21 | SERDES6G_11
        QSGMII, // 30 | DEV2G5_22 | SERDES6G_11
        QSGMII, // 31 | DEV2G5_23 | SERDES6G_11
        QSGMII, // 32 | DEV1G_8   | SERDES6G_12
        QSGMII, // 33 | DEV1G_9   | SERDES6G_12
        QSGMII, // 34 | DEV1G_10  | SERDES6G_12
        QSGMII, // 35 | DEV1G_11  | SERDES6G_12
        QSGMII, // 36 | DEV1G_12  | SERDES6G_13
        QSGMII, // 37 | DEV1G_13  | SERDES6G_13
        QSGMII, // 38 | DEV1G_14  | SERDES6G_13
        QSGMII, // 39 | DEV1G_15  | SERDES6G_13
        QSGMII, // 40 | DEV1G_16  | SERDES6G_14
        QSGMII, // 41 | DEV1G_17  | SERDES6G_14
        QSGMII, // 42 | DEV1G_18  | SERDES6G_14
        QSGMII, // 43 | DEV1G_19  | SERDES6G_14
        QSGMII, // 44 | DEV1G_20  | SERDES6G_15
        QSGMII, // 45 | DEV1G_21  | SERDES6G_15
        QSGMII, // 46 | DEV1G_22  | SERDES6G_15
        QSGMII, // 47 | DEV1G_23  | SERDES6G_15
        None,   // 48 | Unused (NPI)
        SFI,    // 49 | DEV10G_0  | SERDES10G_0 | OTS switch
        SFI,    // 50 | DEV10G_0  | SERDES10G_0 | OTS switch
        SGMII,  // 51 | DEV2G5_27 | SERDES10G_2 | mgmt bringup board
        SGMII,  // 52 | DEV2G5_28 | SERDES10G_3 | mgmt bringup board
    ]);
}
pub use map::PORT_MAP;

pub struct Bsp<'a, R> {
    vsc7448: &'a Vsc7448<'a, R>,
    vsc8522: [Vsc8522; 4],
    leds: UserLeds,
    phy_link_up: [[bool; 24]; 2],
}

impl<'a, R: Vsc7448Rw> Bsp<'a, R> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448<'a, R>) -> Result<Self, VscError> {
        let leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());
        let mut out = Bsp {
            vsc7448,
            vsc8522: [Vsc8522::empty(); 4], // To be populated with phy_init()
            leds,
            phy_link_up: Default::default(),
        };
        out.reinit()?;
        Ok(out)
    }

    /// Initializes the four PHYs on the dev kit
    fn phy_init(&mut self) -> Result<(), VscError> {
        // The VSC7448 dev kit has 2x VSC8522 PHYs on each of MIIM1 and MIIM2.
        // Each PHYs on the same MIIM bus is strapped to different ports.
        let mut i = 0;
        for miim in [1, 2] {
            self.vsc7448
                .modify(DEVCPU_GCB().MIIM(miim).MII_CFG(), |cfg| {
                    cfg.set_miim_cfg_prescale(0xFF)
                })?;
            // We only need to check this on one PHY port per physical PHY
            // chip.  Port 0 maps to one PHY chip, and port 12 maps to the
            // other one (controlled by hardware pull-ups).
            let phy_rw = &mut Vsc7448MiimPhy::new(self.vsc7448, miim);
            for port in [0, 12] {
                self.vsc8522[i] = Vsc8522::init(port, phy_rw)?;
                i += 1;
            }
        }
        Ok(())
    }

    fn gpio_init(&self) -> Result<(), VscError> {
        // The VSC7448 dev kit has PHYs on MIIM1 and MIIM2, so we configure them
        // by setting GPIO_56-59 to Overlay Function 1.
        self.vsc7448
            .write(DEVCPU_GCB().GPIO().GPIO_ALT1(0), 0xF000000.into())?;
        Ok(())
    }

    /// Attempts to initialize the system.  This is based on a VSC7448 dev kit
    /// (VSC5627EV), so will need to change depending on your system.
    pub fn reinit(&mut self) -> Result<(), VscError> {
        self.leds.led_off(3).unwrap();
        self.leds.led_on(0).unwrap();

        self.vsc7448.init()?;
        self.gpio_init()?;
        self.phy_init()?;

        self.vsc7448.configure_ports_from_map(&PORT_MAP)?;
        self.vsc7448.configure_vlan_optional()?;

        self.leds.led_off(0).unwrap();
        self.leds.led_on(3).unwrap();
        Ok(())
    }

    /// Checks the given PHY's status, return `true` if the link is up
    fn check_phy(&mut self, miim: u8, phy: u8) -> bool {
        let phy_rw = &mut Vsc7448MiimPhy::new(self.vsc7448, miim);
        let p = Phy::new(phy, phy_rw);
        match p.read(phy::STANDARD::MODE_STATUS()) {
            Ok(status) => {
                let up = (status.0 & (1 << 5)) != 0;
                if up != self.phy_link_up[miim as usize - 1][phy as usize] {
                    self.phy_link_up[miim as usize - 1][phy as usize] = up;
                    ringbuf_entry!(Trace::PhyLinkChanged {
                        port: (miim - 1) * 24 + phy,
                        status: status.0,
                    });
                }
                up
            }
            Err(err) => {
                ringbuf_entry!(Trace::PhyScanError { miim, phy, err });
                false
            }
        }
    }

    fn check_sgmii(&mut self, dev: u8) -> bool {
        match self
            .vsc7448
            .read(DEV2G5(dev).PCS1G_CFG_STATUS().PCS1G_LINK_STATUS())
        {
            Ok(v) => v.link_status() != 0,
            Err(err) => {
                ringbuf_entry!(Trace::SgmiiError { dev, err });
                false
            }
        }
    }

    pub fn wake(&mut self) -> Result<(), VscError> {
        let mut any_phy_up = false;
        for miim in [1, 2] {
            for phy in 0..24 {
                any_phy_up |= self.check_phy(miim, phy);
            }
        }
        if any_phy_up {
            self.leds.led_on(1).unwrap();
        } else {
            self.leds.led_off(1).unwrap();
        }

        // Check the DEV2G5 ports that could be mapped to SGMII on SFP slots
        let mut any_sgmii_up = false;
        for d in 25..29 {
            any_sgmii_up |= self.check_sgmii(d);
        }
        if any_sgmii_up {
            self.leds.led_on(2).unwrap();
        } else {
            self.leds.led_off(2).unwrap();
        }

        Ok(())
    }

    /// Calls a function on a `Phy` associated with the given port.
    ///
    /// Returns `None` if the given port isn't associated with a PHY
    /// (for example, because it's an SGMII link)
    pub fn phy_fn<T, F: Fn(vsc85xx::Phy<Vsc7448MiimPhy<R>>) -> T>(
        &mut self,
        port: u8,
        callback: F,
    ) -> Option<T> {
        let miim = match port {
            0..=23 => 1,
            24..=48 => 2,
            _ => return None,
        };
        let phy_port = port % 24;
        let mut phy_rw: Vsc7448MiimPhy<R> =
            Vsc7448MiimPhy::new(self.vsc7448.rw, miim);
        let phy = vsc85xx::Phy::new(phy_port, &mut phy_rw);
        Some(callback(phy))
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Turns on LEDs to let the user know that the board is alive and starting
/// initialization (we'll turn these off at the end of Bsp::init)
pub fn preinit() {
    let leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());
    leds.led_off(1).unwrap();
    leds.led_off(2).unwrap();
    leds.led_off(3).unwrap();

    leds.led_on(0).unwrap();
}
