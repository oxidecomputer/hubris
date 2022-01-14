use crate::VscError;
use vsc7448_pac::{phy, types::PhyRegisterAddress};

pub trait PhyRw {
    /// Reads a register from the PHY without changing the page.  This should
    /// never be called directly, because the page could be incorrect, but
    /// it's a required building block for `read`
    fn read_raw<T: From<u16>>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError>;

    /// Writes a register to the PHY without changing the page.  This should
    /// never be called directly, because the page could be incorrect, but
    /// it's a required building block for `read` and `write`
    fn write_raw<T>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone;

    fn read<T>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        self.write_raw::<phy::standard::PAGE>(
            phy,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        self.read_raw(phy, reg)
    }

    fn write<T>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        self.write_raw::<phy::standard::PAGE>(
            phy,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        self.write_raw(phy, reg, value)
    }

    /// Performs a read-modify-write operation on a PHY register connected
    /// to the VSC7448 via MIIM.
    fn modify<T, F>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(&mut T),
    {
        let mut data = self.read(phy, reg)?;
        f(&mut data);
        self.write(phy, reg, data)
    }
}

/// Initializes one or more VSC8522 PHYs connected over MIIM
pub fn init_miim_phy<P: PhyRw>(ports: &[u8], v: P) -> Result<(), VscError> {
    for &port in ports {
        // Do a self-reset on the PHY
        v.modify(port, phy::STANDARD::MODE_CONTROL(), |g| g.set_sw_reset(1))?;
        let id1 = v.read(port, phy::STANDARD::IDENTIFIER_1())?.0;
        if id1 != 0x7 {
            return Err(VscError::BadPhyId1(id1));
        }
        let id2 = v.read(port, phy::STANDARD::IDENTIFIER_2())?.0;
        if id2 != 0x6f3 {
            return Err(VscError::BadPhyId2(id2));
        }

        // Disable COMA MODE, which keeps the chip holding itself in reset
        v.modify(port, phy::GPIO::GPIO_CONTROL_2(), |g| {
            g.set_coma_mode_output_enable(0)
        })?;

        // Configure the PHY in QSGMII + 12 port mode
        v.write(port, phy::GPIO::MICRO_PAGE(), 0x80A0.into())?;
    }
    Ok(())
}
