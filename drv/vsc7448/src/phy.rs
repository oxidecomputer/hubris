use crate::{spi::Vsc7448Spi, VscError};
use vsc7448_pac::{phy, Vsc7448};

/// Initializes one or more VSC8522 PHYs connected over MIIM
pub fn init_miim_phy(
    miim: u8,
    ports: &[u8],
    v: &Vsc7448Spi,
) -> Result<(), VscError> {
    v.modify(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_CFG(), |cfg| {
        cfg.set_miim_cfg_prescale(0xFF)
    })?;
    for &port in ports {
        // Do a self-reset on the PHY
        v.phy_modify(miim, port, phy::STANDARD::MODE_CONTROL(), |g| {
            g.set_sw_reset(1)
        })?;
        let id1 = v.phy_read(miim, port, phy::STANDARD::IDENTIFIER_1())?.0;
        if id1 != 0x7 {
            return Err(VscError::BadPhyId1(id1));
        }
        let id2 = v.phy_read(miim, port, phy::STANDARD::IDENTIFIER_2())?.0;
        if id2 != 0x6f3 {
            return Err(VscError::BadPhyId2(id2));
        }

        // Disable COMA MODE, which keeps the chip holding itself in reset
        v.phy_modify(miim, port, phy::GPIO::GPIO_CONTROL_2(), |g| {
            g.set_coma_mode_output_enable(0)
        })?;

        // Configure the PHY in QSGMII + 12 port mode
        v.phy_write(miim, port, phy::GPIO::MICRO_PAGE(), 0x80A0.into())?;
    }
    Ok(())
}
