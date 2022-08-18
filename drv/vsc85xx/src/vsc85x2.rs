// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Counter, Phy, PhyRw, VscError};
use vsc7448_pac::phy;

// These IDs are (id1 << 16) | id2, meaning they also capture device revision
// number.  This matters, because the patches are device-revision specific.
pub const VSC8552_ID: u32 = 0x704e2;

// The datasheet will tell you that the ID for the VSC8562 should be 0x707b1.
// Don't believe its lies!  The SDK (as the one source of truth) informs us
// that it shares an ID with the VSC8564, then has a secondary ID in the
// EXTENDED_CHIP_ID register
pub const VSC8562_ID: u32 = 0x707e1;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Vsc85x2Type {
    Vsc8552,
    Vsc8562,
}

/// Represents a VSC8552 or VSC8562 chip.  `base_port` is the PHY address of
/// the chip's port 0; since this is a two-port PHY, we can address either
/// `base_port` or `base_port + 1` given a suitable `PhyRw`.
pub struct Vsc85x2 {
    base_port: u8,
    phy_type: Vsc85x2Type,
}

impl Vsc85x2 {
    pub fn init_sgmii<P: PhyRw>(
        base_port: u8,
        rw: &mut P,
    ) -> Result<Self, VscError> {
        let phy = &mut Phy::new(base_port, rw);
        let phy_type = match phy.read_id()? {
            VSC8552_ID => {
                let rev = phy.read(phy::GPIO::EXTENDED_REVISION())?;
                if rev.tesla_e() == 1 {
                    Vsc85x2Type::Vsc8552
                } else {
                    return Err(VscError::BadPhyRev);
                }
            }
            VSC8562_ID => {
                let rev = phy.read(phy::GPIO::EXTENDED_REVISION())?;
                if u16::from(rev) & 0x4000 == 0 {
                    Vsc85x2Type::Vsc8562
                } else {
                    return Err(VscError::BadPhyRev);
                }
            }
            i => return Err(VscError::UnknownPhyId(i)),
        };
        let out = Self {
            base_port,
            phy_type,
        };
        out.phy(0, rw).init_sgmii()?;
        Ok(out)
    }

    /// Returns a handle to address the specified port, which must be either 0
    /// or 1; this function offsets by the chip's port offset, which is set
    /// by resistor strapping.
    pub fn phy<'a, P: PhyRw>(
        &self,
        port: u8,
        rw: &'a mut P,
    ) -> Vsc85x2Phy<'a, P> {
        assert!(port < 2);
        Vsc85x2Phy {
            phy_type: self.phy_type,
            phy: Phy::new(self.base_port + port, rw),
        }
    }

    /// Sets the SIGDET polarity for all PHYs (by default, active high)
    pub fn set_sigdet_polarity<P: PhyRw>(
        &self,
        rw: &mut P,
        active_low: bool,
    ) -> Result<(), VscError> {
        self.phy(0, rw).phy.broadcast(|phy| {
            phy.modify(phy::EXTENDED::EXTENDED_MODE_CONTROL(), |r| {
                // TODO: fix VSC7448 codegen to include `sigdet_polarity` bit
                let mut v = u16::from(*r);
                v = (v & !1) | active_low as u16;
                *r = v.into();
            })
        })
    }

    pub fn has_mac_counters(&self) -> bool {
        match self.phy_type {
            Vsc85x2Type::Vsc8552 => false,
            Vsc85x2Type::Vsc8562 => true,
        }
    }
}

/// Represents a single PHY within a VSC8552 or VSC8562 chip.  This is a
/// transient `struct`, because the inner `Phy` is likely own something
/// important.
pub struct Vsc85x2Phy<'a, P> {
    phy_type: Vsc85x2Type,
    pub phy: Phy<'a, P>,
}

impl<'a, P: PhyRw> Vsc85x2Phy<'a, P> {
    /// Initializes either a VSC8552 or VSC8562 PHY, configuring it to use 2x
    /// SGMII to 100BASE-FX SFP fiber). This should be called _after_ the PHY
    /// is reset (i.e. the reset pin is toggled and then the caller waits for
    /// 120 ms).  The caller is also responsible for handling the `COMA_MODE`
    /// pin.
    ///
    /// This must be called on the base port of the PHY; otherwise it will
    /// return an error.
    fn init_sgmii(&mut self) -> Result<(), VscError> {
        match self.phy_type {
            Vsc85x2Type::Vsc8552 => {
                crate::vsc8552::Vsc8552Phy { phy: &mut self.phy }.init()
            }
            Vsc85x2Type::Vsc8562 => {
                crate::vsc8562::Vsc8562Phy { phy: &mut self.phy }.init_sgmii()
            }
        }
    }

    fn select_media_counters(&mut self) -> Result<(), VscError> {
        // Configure the PHY to read fiber media SerDes counters
        if self.phy_type == Vsc85x2Type::Vsc8562 {
            self.phy.modify(
                phy::EXTENDED_3::MEDIA_SERDES_TX_CRC_ERROR_COUNTER(),
                |r| r.set_tx_select(0),
            )?;
            self.phy.modify(
                phy::EXTENDED_3::MEDIA_MAC_SERDES_RX_CRC_CRC_ERR_COUNTER(),
                |r| r.0 &= !(0b11 << 14),
            )?;
        }
        Ok(())
    }

    /// Configure the PHY to read MAC side counters.
    ///
    /// This may only be called on a VSC8562 PHY; it will panic otherwise.
    fn select_mac_counters(&mut self) -> Result<(), VscError> {
        assert_eq!(self.phy_type, Vsc85x2Type::Vsc8562);

        self.phy.modify(
            phy::EXTENDED_3::MEDIA_SERDES_TX_CRC_ERROR_COUNTER(),
            |r| r.set_tx_select(1),
        )?;
        self.phy.modify(
            phy::EXTENDED_3::MEDIA_MAC_SERDES_RX_CRC_CRC_ERR_COUNTER(),
            |r| {
                r.0 &= !(0b11 << 14);
                r.0 |= 0b01 << 14;
            },
        )?;
        Ok(())
    }

    pub fn mac_tx_rx_good(&mut self) -> Result<(Counter, Counter), VscError> {
        if self.phy_type == Vsc85x2Type::Vsc8552 {
            return Ok((Counter::Unavailable, Counter::Unavailable));
        }
        self.select_mac_counters()?;
        self.tx_rx_good()
    }

    pub fn media_tx_rx_good(&mut self) -> Result<(Counter, Counter), VscError> {
        self.select_media_counters()?;
        self.tx_rx_good()
    }

    pub fn mac_tx_rx_bad(&mut self) -> Result<(Counter, Counter), VscError> {
        if self.phy_type == Vsc85x2Type::Vsc8552 {
            return Ok((Counter::Unavailable, Counter::Unavailable));
        }
        self.select_mac_counters()?;
        self.tx_rx_bad()
    }

    pub fn media_tx_rx_bad(&mut self) -> Result<(Counter, Counter), VscError> {
        self.select_media_counters()?;
        self.tx_rx_bad()
    }

    fn tx_rx_good(&mut self) -> Result<(Counter, Counter), VscError> {
        let r = self
            .phy
            .read(phy::EXTENDED_3::MEDIA_SERDES_TX_GOOD_PACKET_COUNTER())?;
        let tx = if r.active() == 0 {
            Counter::Inactive
        } else {
            Counter::Value(r.cnt())
        };
        let r = self
            .phy
            .read(phy::EXTENDED_3::MEDIA_MAC_SERDES_RX_GOOD_COUNTER())?;
        let rx = Counter::Value(r.cnt());

        Ok((tx, rx))
    }

    fn tx_rx_bad(&mut self) -> Result<(Counter, Counter), VscError> {
        let r = self
            .phy
            .read(phy::EXTENDED_3::MEDIA_SERDES_TX_CRC_ERROR_COUNTER())?;
        let tx = Counter::Value(r.cnt());
        let r = self
            .phy
            .read(phy::EXTENDED_3::MEDIA_MAC_SERDES_RX_CRC_CRC_ERR_COUNTER())?;
        let rx = Counter::Value(r.0 & 0xFF);

        Ok((tx, rx))
    }
}
