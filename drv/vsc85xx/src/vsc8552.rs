// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::led::*;
use crate::{Phy, PhyRw, Trace, VscError};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use vsc7448_pac::phy;

pub struct Vsc8552Phy<'a, 'b, P> {
    pub phy: &'b mut Phy<'a, P>,
}

impl<'a, 'b, P: PhyRw> Vsc8552Phy<'a, 'b, P> {
    /// Initializes a VSC8552 PHY using SGMII based on section 3.1.2 (2x SGMII
    /// to 100BASE-FX SFP Fiber).  Same caveats as `init` apply.
    pub fn init(&mut self) -> Result<(), VscError> {
        ringbuf_entry!(Trace::Vsc8552Init(self.phy.port));
        self.phy.check_base_port()?;

        // Apply a patch to the 8051 inside the PHY
        crate::tesla::TeslaPhy { phy: self.phy }.patch()?;

        self.phy.broadcast(|v| {
            v.modify(phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
                // MAC configuration = SGMII
                r.0 &= !(0b11 << 14)
            })
        })?;

        // Enable 2 port MAC SGMII, then wait for the command to finish
        self.phy.cmd(0x80F0)?;

        self.phy.broadcast(|v| {
            v.modify(phy::STANDARD::EXTENDED_PHY_CONTROL(), |r| {
                // SGMII MAC interface mode
                r.set_mac_interface_mode(0);
                // 100BASE-FX fiber/SFP on the fiber media pins only
                r.set_media_operating_mode(0b11);
            })
        })?;

        // Enable 2 ports Media 100BASE-FX
        self.phy.cmd(0x8FD1)?;

        // Configure LEDs.
        self.phy.broadcast(|v| {
            v.set_led_mode(LED::LED0, LEDMode::ForcedOff)?;
            v.set_led_mode(
                LED::LED1,
                LEDMode::Link100BaseFXLink1000BaseXActivity,
            )?;
            v.set_led_mode(LED::LED2, LEDMode::Activity)?;
            v.set_led_mode(LED::LED3, LEDMode::Fiber100Fiber1000Activity)?;

            // Tweak LED behavior.
            v.modify(phy::STANDARD::LED_BEHAVIOR(), |r| {
                let x: u16 = (*r).into();
                // Disable LED1 combine, showing only link status.
                let disable_led1_combine = 1 << 1;
                // Split TX/RX activity across Activity/FiberActivity modes.
                let split_rx_tx_activity = 1 << 14;
                *r = phy::standard::LED_BEHAVIOR::from(
                    x | disable_led1_combine | split_rx_tx_activity,
                );
            })?;

            // Enable the link state change mask, to detect PHY link flapping
            v.modify(phy::STANDARD::INTERRUPT_MASK(), |r| r.set_link_mask(1))?;

            Ok(())
        })?;

        // Now, we reset the PHY to put those settings into effect.  For some
        // reason, we can't do a broadcast reset, so we do it port-by-port.
        for p in 0..2 {
            Phy::new(self.phy.port + p, self.phy.rw).software_reset()?;
        }
        Ok(())
    }
}
