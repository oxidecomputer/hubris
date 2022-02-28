// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_user_leds_api::UserLeds;
use ringbuf::*;
use userlib::*;
use vsc7448::{miim_phy::Vsc7448MiimPhy, Vsc7448, Vsc7448Rw, VscError};
use vsc7448_pac::{phy, *};
use vsc85xx::{init_vsc8522_phy, Phy};

task_slot!(USER_LEDS, user_leds);

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

pub struct Bsp<'a, R> {
    vsc7448: &'a Vsc7448<'a, R>,
    leds: UserLeds,
    phy_link_up: [[bool; 24]; 2],
    known_macs: [Option<[u8; 6]>; 16],
}

impl<'a, R: Vsc7448Rw> Bsp<'a, R> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448<'a, R>) -> Result<Self, VscError> {
        let leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());
        let out = Bsp {
            vsc7448,
            leds,
            phy_link_up: Default::default(),
            known_macs: Default::default(),
        };
        out.init()?;
        Ok(out)
    }

    /// Initializes the four PHYs on the dev kit
    fn phy_init(&self) -> Result<(), VscError> {
        // The VSC7448 dev kit has 2x VSC8522 PHYs on each of MIIM1 and MIIM2.
        // Each PHYs on the same MIIM bus is strapped to different ports.
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
                let mut p = Phy::new(port, phy_rw);
                init_vsc8522_phy(&mut p)?;
            }
        }
        Ok(())
    }

    fn gpio_init(&self) -> Result<(), VscError> {
        // We assume that the only person running on a gemini-bu-1 is Matt, who is
        // talking to a VSC7448 dev kit on his desk.  In this case, we want to
        // configure the GPIOs to allow MIIM1 and 2 to be active, by setting
        // GPIO_56-59 to Overlaid Function 1
        self.vsc7448
            .write(DEVCPU_GCB().GPIO().GPIO_ALT1(0), 0xF000000.into())?;
        Ok(())
    }

    /// Attempts to initialize the system.  This is based on a VSC7448 dev kit
    /// (VSC5627EV), so will need to change depending on your system.
    fn init(&self) -> Result<(), VscError> {
        self.gpio_init()?;
        self.phy_init()?;

        self.vsc7448
            .init_qsgmii(&[0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44])?;
        self.vsc7448.init_sfi(&[49, 50])?;
        self.vsc7448.init_10g_sgmii(&[51, 52])?;

        self.vsc7448.apply_calendar()?;

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
