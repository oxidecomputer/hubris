// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Phy, PhyRw, VscError};
use vsc7448_pac::phy;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LED {
    LED0 = 0,
    LED1,
    LED2,
    LED3,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LEDMode {
    LinkActivity = 0,
    Link1000Activity,
    Link100Activity,
    Link10Activity,
    Link100Link1000Activity,
    Link10Link1000Activity,
    Link10Link100Activity,
    Link100BaseFXLink1000BaseXActivity,
    DuplexCollision,
    Collision,
    Activity,
    Fiber100Fiber1000Activity,
    AutonegotiationFault,
    SerialMode,
    ForcedOff,
    ForcedOn,
}

impl<'a, P: PhyRw> Phy<'a, P> {
    pub fn set_led_mode(
        &self,
        led: LED,
        mode: LEDMode,
    ) -> Result<(), VscError> {
        self.modify(phy::STANDARD::LED_MODE_SELECT(), |r| {
            let shift_amount = led as u8 * 4;
            r.0 = (r.0 & !(0xf << shift_amount))
                | ((mode as u16) << shift_amount);
        })
    }
}
