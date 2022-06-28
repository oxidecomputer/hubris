// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Phy, PhyRw, Trace, VscError};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use vsc7448_pac::phy;

pub const VSC8522_ID: u32 = 0x706f3;

/// Represents a VSC8522, which is a 12-port PHY used on the VSC7448 dev kit.
/// `base_port` is the PHY address of the chip's port 0.
#[derive(Copy, Clone, Debug)]
pub struct Vsc8522 {
    base_port: u8,
}

impl Vsc8522 {
    /// Constructs an invalid Vsc8522, which will panic if you call the
    /// `phy()` function.
    pub fn empty() -> Self {
        Self { base_port: 0xFF }
    }

    /// Initializes a VSC8522 PHY using QSGMII.
    pub fn init<P: PhyRw>(base_port: u8, rw: &mut P) -> Result<Self, VscError> {
        let out = Self { base_port };
        out.phy(0, rw).init()?;

        Ok(out)
    }

    /// Returns a handle to address the specified port, which must be in the
    /// range 0-11; this function offsets by the chip's port offset, which is
    /// set by resistor strapping and stored in `self.base_port`.
    pub fn phy<'a, P: PhyRw>(
        &self,
        port: u8,
        rw: &'a mut P,
    ) -> Vsc8522Phy<'a, P> {
        assert!(port < 12);
        assert!(self.base_port != 0xFF);
        Vsc8522Phy {
            phy: Phy::new(self.base_port + port, rw),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct Vsc8522Phy<'a, P> {
    pub phy: Phy<'a, P>,
}

impl<'a, P: PhyRw> Vsc8522Phy<'a, P> {
    fn init(&mut self) -> Result<(), VscError> {
        ringbuf_entry!(Trace::Vsc8522Init(self.phy.port));

        let id = self.phy.read_id()?;
        if id != VSC8522_ID {
            return Err(VscError::BadPhyId(id));
        }

        // Disable COMA MODE, which keeps the chip holding itself in reset
        self.phy.modify(phy::GPIO::GPIO_CONTROL_2(), |g| {
            g.set_coma_mode_output_enable(0)
        })?;

        // Configure the PHY in QSGMII + 12 port mode
        self.phy.cmd(0x80A0)?;

        // Enable MAC autonegotiation
        self.phy.broadcast(|p| {
            p.modify(
                phy::EXTENDED_3::MAC_SERDES_PCS_CONTROL(),
                |g| g.0 |= 1 << 7, // Enable MAC SerDes autonegotiation
            )
        })?;

        Ok(())
    }
}
