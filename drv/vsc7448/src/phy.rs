use crate::VscError;
use userlib::hl::sleep_for;
use vsc7448_pac::{phy, types::PhyRegisterAddress};

/// Trait implementing communication with an ethernet PHY.
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

/// Initializes a VSC8522 PHY using QSGMII
pub fn init_vsc8522_phy<P: PhyRw>(port: u8, v: &P) -> Result<(), VscError> {
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
    Ok(())
}

/// Initializes a VSC8504 PHY using QSGMII, based on the "Configuration"
/// guide in the datasheet (section 3.19).
pub fn init_vsc8504_phy<P: PhyRw>(port: u8, v: &P) -> Result<(), VscError> {
    // The caller should toggle the reset pin and wait 120 ms

    // TODO: apply PHY_API patch

    let id1 = v.read(port, phy::STANDARD::IDENTIFIER_1())?.0;
    if id1 != 0x7 {
        return Err(VscError::BadPhyId1(id1));
    }
    let id2 = v.read(port, phy::STANDARD::IDENTIFIER_2())?.0;
    if id2 != 0x4c2 {
        return Err(VscError::BadPhyId2(id2));
    }

    v.modify(port, phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
        r.0 |= 0b01 << 14; // QSGMII
    })?;

    // Enable 4 port MAC QSGMII
    v.write(port, phy::GPIO::MICRO_PAGE(), 0x80E0.into())?;

    // Wait for the PHY to be ready
    let mut ready = false;
    for _ in 0..32 {
        if (v.read(port, phy::GPIO::MICRO_PAGE())?.0 & (1 << 15)) != 0 {
            ready = true;
            break;
        }
        sleep_for(1);
    }
    if !ready {
        return Err(VscError::PhyInitTimeout);
    }

    // The PHY is already configured for copper in register 23
    // TODO: check that this is correct

    // Now, we reset the PHY and wait for the bit to clear
    v.modify(port, phy::STANDARD::MODE_CONTROL(), |r| {
        r.set_sw_reset(1);
    })?;
    let mut ready = false;
    for _ in 0..32 {
        if v.read(port, phy::STANDARD::MODE_CONTROL())?.sw_reset() != 1 {
            ready = true;
            break;
        }
        sleep_for(1);
    }

    Ok(())
}
