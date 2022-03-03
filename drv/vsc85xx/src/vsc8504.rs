// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Phy, PhyRw, Trace, VscError};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use vsc7448_pac::phy;

const VSC8504_ID: u32 = 0x704c2;

/// Represents a VSC8504, which is a 4-port PHY used on Sidecar.
/// `base_port` is the PHY address of the chip's port 0.
pub struct Vsc8504 {
    base_port: u8,
}

impl Vsc8504 {
    /// Constructs an invalid Vsc8504, which will panic if you call the
    /// `phy()` function.
    pub fn empty() -> Self {
        Self { base_port: 0xFF }
    }

    /// Initializes a VSC8504 PHY using QSGMII, based on the "Configuration"
    /// guide in the datasheet (section 3.19).  This should be called _after_
    /// the PHY is reset (i.e. the reset pin is toggled and then the caller
    /// waits for 120 ms).  The caller is also responsible for handling the
    /// `COMA_MODE` pin.
    ///
    /// This must be called on the base port of the PHY, and will configure all
    /// ports using broadcast writes.
    pub fn init<P: PhyRw>(base_port: u8, rw: &mut P) -> Result<Self, VscError> {
        let out = Self { base_port };
        out.phy(0, rw).init()?;

        Ok(out)
    }

    /// Returns a handle to address the specified port, which must be in the
    /// range 0-3; this function offsets by the chip's port offset, which is
    /// set by resistor strapping and stored in `self.base_port`.
    pub fn phy<'a, P: PhyRw>(
        &self,
        port: u8,
        rw: &'a mut P,
    ) -> Vsc8504Phy<'a, P> {
        assert!(port < 4);
        assert!(self.base_port != 0xFF);
        Vsc8504Phy {
            phy: Phy::new(self.base_port + port, rw),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct Vsc8504Phy<'a, P> {
    pub phy: Phy<'a, P>,
}

impl<'a, P: PhyRw> Vsc8504Phy<'a, P> {
    fn init(&mut self) -> Result<(), VscError> {
        ringbuf_entry!(Trace::Vsc8504Init(self.phy.port));

        let id = self.phy.read_id()?;
        if id != VSC8504_ID {
            return Err(VscError::BadPhyId(id));
        }

        let rev = self.phy.read(phy::GPIO::EXTENDED_REVISION())?;
        if rev.tesla_e() != 1 {
            return Err(VscError::BadPhyRev);
        }

        self.phy.check_base_port()?;
        crate::tesla::TeslaPhy { phy: &mut self.phy }.patch()?;

        // Configure MAC in QSGMII mode
        self.phy.broadcast(|v| {
            v.modify(phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
                r.0 = (r.0 & !(0b11 << 14)) | (0b01 << 14)
            })
        })?;

        // Enable 4 port MAC QSGMII
        self.phy.cmd(0x80E0)?;

        // The PHY is already configured for copper in register 23
        // XXX: I don't think this is correct

        // Now, we reset the PHY to put those settings into effect
        // XXX: is it necessary to reset each of the four ports independently?
        // (It _is_ necessary for the VSC8552 on the management network dev board)
        for p in 0..4 {
            Phy::new(self.phy.port + p, self.phy.rw).software_reset()?;
        }

        Ok(())
    }
}
