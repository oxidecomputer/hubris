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

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    PhyScanError { miim: u8, phy: u8, err: VscError },
    PhyLinkChanged { port: u8, status: u16 },
    SgmiiError { dev: u8, err: VscError },
    MacAddress(vsc7448::mac::MacTableEntry),
    VscErr(VscError),
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

mod map {
    // See RFD144 for a detailed look at the design
    use vsc7448::status::{PortMap, PortMode::*};
    use vsc7448::Speed::*;
    pub const DEV_KIT_MAP: PortMap = PortMap {
        ports: [
            Some(Qsgmii(Speed1G)), //  0  | DEV1G_0
            Some(Qsgmii(Speed1G)), //  1  | DEV1G_1
            Some(Qsgmii(Speed1G)), //  2  | DEV1G_2
            Some(Qsgmii(Speed1G)), //  3  | DEV1G_3
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  4  | DEV1G_4
            Some(Qsgmii(Speed1G)), //  5  | DEV1G_5
            Some(Qsgmii(Speed1G)), //  6  | DEV1G_6
            Some(Qsgmii(Speed1G)), //  7  | DEV1G_7
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  8  | DEV2G5_0
            Some(Qsgmii(Speed1G)), //  9  | DEV2G5_1
            Some(Qsgmii(Speed1G)), //  10 | DEV2G5_2
            Some(Qsgmii(Speed1G)), //  11 | DEV2G5_3
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  12 | DEV2G5_4
            Some(Qsgmii(Speed1G)), //  13 | DEV2G5_5
            Some(Qsgmii(Speed1G)), //  14 | DEV2G5_6
            Some(Qsgmii(Speed1G)), //  15 | DEV2G5_7
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  16 | DEV2G5_8
            Some(Qsgmii(Speed1G)), //  17 | DEV2G5_9
            Some(Qsgmii(Speed1G)), //  18 | DEV2G5_10
            Some(Qsgmii(Speed1G)), //  19 | DEV2G5_11
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  20 | DEV2G5_12
            Some(Qsgmii(Speed1G)), //  21 | DEV2G5_13
            Some(Qsgmii(Speed1G)), //  22 | DEV2G5_14
            Some(Qsgmii(Speed1G)), //  23 | DEV2G5_15
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  24 | DEV2G5_16
            Some(Qsgmii(Speed1G)), //  25 | DEV2G5_17
            Some(Qsgmii(Speed1G)), //  26 | DEV2G5_18
            Some(Qsgmii(Speed1G)), //  27 | DEV2G5_19
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  28 | DEV2G5_20
            Some(Qsgmii(Speed1G)), //  29 | DEV2G5_21
            Some(Qsgmii(Speed1G)), //  30 | DEV2G5_22
            Some(Qsgmii(Speed1G)), //  31 | DEV2G5_23
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  32 | DEV1G_8
            Some(Qsgmii(Speed1G)), //  33 | DEV1G_9
            Some(Qsgmii(Speed1G)), //  34 | DEV1G_10
            Some(Qsgmii(Speed1G)), //  35 | DEV1G_11
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  36 | DEV1G_12
            Some(Qsgmii(Speed1G)), //  37 | DEV1G_13
            Some(Qsgmii(Speed1G)), //  38 | DEV1G_14
            Some(Qsgmii(Speed1G)), //  39 | DEV1G_15
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  40 | DEV1G_16
            Some(Qsgmii(Speed1G)), //  41 | DEV1G_17
            Some(Qsgmii(Speed1G)), //  42 | DEV1G_18
            Some(Qsgmii(Speed1G)), //  43 | DEV1G_19
            ///////////////////////////////////////////////////////////////////
            Some(Qsgmii(Speed1G)), //  44 | DEV1G_20
            Some(Qsgmii(Speed1G)), //  45 | DEV1G_21
            Some(Qsgmii(Speed1G)), //  46 | DEV1G_22
            Some(Qsgmii(Speed1G)), //  47 | DEV1G_23
            ///////////////////////////////////////////////////////////////////
            None,        // 48
            Some(Sfi),   // 49 | DEV10G_0
            Some(Sfi),   // 50 | DEV10G_1
            Some(Sgmii), // 51 | DEV2G5_27
            Some(Sgmii), // 52 | DEV2G5_28
        ],
    };
}
////////////////////////////////////////////////////////////////////////////////
pub struct Bsp<'a, R> {
    vsc7448: &'a Vsc7448<'a, R>,
    vsc8522: [Vsc8522; 4],
    leds: UserLeds,
    phy_link_up: [[bool; 24]; 2],
    known_macs: [Option<[u8; 6]>; 16],
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
            known_macs: Default::default(),
        };
        out.init()?;
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
    fn init(&mut self) -> Result<(), VscError> {
        self.gpio_init()?;
        self.phy_init()?;

        self.vsc7448.configure_ports_from_map(&map::DEV_KIT_MAP)?;
        self.vsc7448.configure_vlan_optional()?;

        self.leds.led_off(0).unwrap();
        self.leds.led_on(3).unwrap();
        Ok(())
    }

    /// Checks the given PHY's status, return `true` if the link is up
    fn check_phy(&mut self, miim: u8, phy: u8) -> bool {
        let phy_rw = &mut Vsc7448MiimPhy::new(self.vsc7448, miim);
        let mut p = Phy::new(phy, phy_rw);
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

    fn wake(&mut self) -> Result<(), VscError> {
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

        // Dump the MAC tables
        while let Some(mac) = vsc7448::mac::next_mac(self.vsc7448)? {
            // Inefficient but easy way to avoid logging MAC addresses
            // repeatedly.  This will fail to scale for larger systems,
            // where we'd want some kind of LRU cache, but is nice
            // for debugging.
            let mut mac_is_new = true;
            for m in self.known_macs.iter_mut() {
                match m {
                    Some(m) => {
                        if *m == mac.mac {
                            mac_is_new = false;
                            break;
                        }
                    }
                    None => {
                        *m = Some(mac.mac);
                        break;
                    }
                }
            }
            if mac_is_new {
                ringbuf_entry!(Trace::MacAddress(mac));
            }
        }
        Ok(())
    }

    pub fn run(&mut self) -> ! {
        loop {
            hl::sleep_for(500);
            if let Err(e) = self.wake() {
                ringbuf_entry!(Trace::VscErr(e));
            }
        }
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
