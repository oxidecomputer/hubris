// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This crate provides functions for working with VSC85xx PHYs
//! (in particular, the VSC8522, VSC8504, and VSC8552)
//!
//! It relies heavily on the trait [PhyRw], which callers must implement.  This
//! trait is an abstraction over reading and writing raw PHY registers.
#![no_std]

use ringbuf::*;
use userlib::hl::sleep_for;
use vsc7448_pac::{phy, types::PhyRegisterAddress};
pub use vsc_err::VscError;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Vsc8522Init(u8),
    Vsc8552Patch(u8),
    Vsc8552Init(u8),
    Vsc8504Init(u8),
    PatchState { patch_ok: bool, skip_download: bool },
    GotCrc(u16),
}
ringbuf!(Trace, 16, Trace::None);

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum LED {
    LED0 = 0,
    LED1,
    LED2,
    LED3,
}

#[derive(Copy, Clone, Debug, PartialEq)]
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

/// Trait implementing communication with an ethernet PHY.
pub trait PhyRw {
    /// Reads a register from the PHY without changing the page.  This should
    /// never be called directly, because the page could be incorrect, but
    /// it's a required building block for `read`
    fn read_raw<T: From<u16>>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError>;

    /// Writes a register to the PHY without changing the page.  This should
    /// never be called directly, because the page could be incorrect, but
    /// it's a required building block for `read` and `write`
    fn write_raw<T>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone;
}

/// Handle for interacting with a particular PHY port
pub struct Phy<'a, P> {
    pub port: u8,
    pub rw: &'a mut P,
}

impl<P: PhyRw> Phy<'_, P> {
    pub fn read<T>(&mut self, reg: PhyRegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        self.rw.write_raw::<phy::standard::PAGE>(
            self.port,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        self.rw.read_raw(self.port, reg)
    }

    pub fn write<T>(
        &mut self,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        self.rw.write_raw::<phy::standard::PAGE>(
            self.port,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        self.rw.write_raw(self.port, reg, value)
    }

    pub fn write_with<T, F>(
        &mut self,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(&mut T),
    {
        let mut data = 0.into();
        f(&mut data);
        self.write(reg, data)
    }

    /// Performs a read-modify-write operation on a PHY register connected
    /// to the VSC7448 via MIIM.
    pub fn modify<T, F>(
        &mut self,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(&mut T),
    {
        let mut data = self.read(reg)?;
        f(&mut data);
        self.write(reg, data)
    }

    pub fn wait_timeout<T, F>(
        &mut self,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(T) -> bool,
    {
        for _ in 0..32 {
            let r = self.read(reg)?;
            if f(r) {
                return Ok(());
            }
            sleep_for(1)
        }
        Err(VscError::PhyInitTimeout)
    }
}

/// Initializes a VSC8522 PHY using QSGMII.
/// This is the PHY on the VSC7448 dev kit.
pub fn init_vsc8522_phy<P: PhyRw + PhyVsc85xx>(
    v: &mut Phy<P>,
) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8522Init(v.port));

    // Do a self-reset on the PHY
    v.modify(phy::STANDARD::MODE_CONTROL(), |g| g.set_sw_reset(1))?;
    let id1 = v.read(phy::STANDARD::IDENTIFIER_1())?.0;
    if id1 != 0x7 {
        return Err(VscError::BadPhyId1(id1));
    }

    let id2 = v.read(phy::STANDARD::IDENTIFIER_2())?.0;
    if id2 != 0x6f3 {
        return Err(VscError::BadPhyId2(id2));
    }

    // Disable COMA MODE, which keeps the chip holding itself in reset
    v.modify(phy::GPIO::GPIO_CONTROL_2(), |g| {
        g.set_coma_mode_output_enable(0)
    })?;

    // Configure the PHY in QSGMII + 12 port mode
    v.cmd(0x80A0)?;
    Ok(())
}

/// Initializes a VSC8504 PHY using QSGMII, based on the "Configuration"
/// guide in the datasheet (section 3.19).  This should be called _after_
/// the PHY is reset (i.e. the reset pin is toggled and then the caller
/// waits for 120 ms).  The caller is also responsible for handling the
/// `COMA_MODE` pin.
pub fn init_vsc8504_phy<P: PhyRw + PhyVsc85xx>(
    v: &mut Phy<P>,
) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8504Init(v.port));

    let id1 = v.read(phy::STANDARD::IDENTIFIER_1())?.0;
    if id1 != 0x7 {
        return Err(VscError::BadPhyId1(id1));
    }
    let id2 = v.read(phy::STANDARD::IDENTIFIER_2())?.0;
    if id2 != 0x4c2 {
        return Err(VscError::BadPhyId2(id2));
    }
    let rev = v.read(phy::GPIO::EXTENDED_REVISION())?;
    if rev.tesla_e() != 1 {
        return Err(VscError::BadPhyRev);
    }

    v.patch()?;

    v.modify(phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
        r.0 |= 0b01 << 14; // QSGMII
    })?;

    // Enable 4 port MAC QSGMII
    v.cmd(0x80E0)?;

    // The PHY is already configured for copper in register 23
    // XXX: I don't think this is correct

    // Now, we reset the PHY and wait for the bit to clear
    v.modify(phy::STANDARD::MODE_CONTROL(), |r| {
        r.set_sw_reset(1);
    })?;
    v.wait_timeout(phy::STANDARD::MODE_CONTROL(), |r| r.sw_reset() != 1)?;

    Ok(())
}

/// Checks the chip ID of a VSC8552 patch, then applies a patch to the built-in
/// 8051 processor based on the MESA SDK.  This must only be called on port 0
/// in the PHY; otherwise it will return an error
///
/// This should be called _after_ the PHY is reset
/// (i.e. the reset pin is toggled and then the caller waits for 120 ms).
/// The caller is also responsible for handling the `COMA_MODE` pin.
pub fn patch_vsc8552_phy<P: PhyRw + PhyVsc85xx>(
    v: &mut Phy<P>,
) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8552Patch(v.port));

    let id1 = v.read(phy::STANDARD::IDENTIFIER_1())?.0;
    if id1 != 0x7 {
        return Err(VscError::BadPhyId1(id1));
    }
    let id2 = v.read(phy::STANDARD::IDENTIFIER_2())?.0;
    if id2 != 0x4e2 {
        return Err(VscError::BadPhyId2(id2));
    }
    let rev = v.read(phy::GPIO::EXTENDED_REVISION())?;
    if rev.tesla_e() != 1 {
        return Err(VscError::BadPhyRev);
    }

    v.patch()
}

/// Initializes a VSC8552 PHY using SGMII based on section 3.1.2 (2x SGMII
/// to 100BASE-FX SFP Fiber). This should be called _after_ [patch_vsc8552_phy],
/// and has the same caveats w.r.t. the reset and COMA_MODE pins.
pub fn init_vsc8552_phy<P: PhyRw + PhyVsc85xx>(
    v: &mut Phy<P>,
) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8552Init(v.port));

    v.modify(phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
        // MAC configuration = SGMII
        r.0 &= !(0b11 << 14);
    })?;
    v.modify(phy::STANDARD::EXTENDED_PHY_CONTROL(), |r| {
        r.set_mac_interface_mode(0); // SGMII
    })?;

    // Enable 2 port MAC SGMII, then wait for the command to finish
    v.cmd(0x80F0)?;

    v.modify(phy::STANDARD::EXTENDED_PHY_CONTROL(), |r| {
        // 100BASE-FX fiber/SFP on the fiber media pins only
        r.set_media_operating_mode(0b011);
    })?;
    v.modify(phy::STANDARD::MODE_CONTROL(), |r| {
        // We have to edit some non-standard bits, so we manipulate the u16
        // directly then convert back.
        let mut v = u16::from(*r);
        v |= 1 << 8; // Full duplex

        // Select 100M speed to 0b01
        v |= 1 << 13; // Set LSB of forced speed selection
        v &= !(1 << 6); // Clear MSB of forced speed selection

        *r = v.into();
        r.set_auto_neg_ena(0);
    })?;

    // Enable 2 ports Media 100BASE-FX
    v.cmd(0x8FD1)?;

    // Configure LEDs.
    v.set_led_mode(LED::LED0, LEDMode::ForcedOff)?;
    v.set_led_mode(LED::LED1, LEDMode::Link100BaseFXLink1000BaseXActivity)?;
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

    // Now, we reset the PHY and wait for the bit to clear
    v.modify(phy::STANDARD::MODE_CONTROL(), |r| {
        r.set_sw_reset(1);
    })?;
    v.wait_timeout(phy::STANDARD::MODE_CONTROL(), |r| r.sw_reset() != 1)?;

    Ok(())
}

/// Marker trait which indicates that a [PhyRw] can use `vsc85xx` commands
pub trait PhyVsc85xx {}

impl<P: PhyRw + PhyVsc85xx> Phy<'_, P> {
    /// The VSC85xx family supports sending commands to the system by writing to
    /// register 19G.  This helper function sends a command then waits for it
    /// to finish, return [VscError::PhyInitTimeout] if it fails (or another
    /// [VscError] if communication to the PHY doesn't work)
    fn cmd(&mut self, command: u16) -> Result<(), VscError> {
        self.write(phy::GPIO::MICRO_PAGE(), command.into())?;
        self.wait_timeout(phy::GPIO::MICRO_PAGE(), |r| r.0 & 0x8000 == 0)?;
        Ok(())
    }

    /// Applies a patch to the 8051 microcode inside the PHY, based on
    /// `vtss_phy_pre_init_seq_tesla_rev_e` in the SDK
    fn patch(&mut self) -> Result<(), VscError> {
        // Enable broadcast flag to configure all ports simultaneously
        self.modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS_20(), |r| {
            r.0 |= 1; // SMI broadcast write
        })?;

        self.write(phy::STANDARD::EXTENDED_PHY_CONTROL_2(), 0x0040.into())?;
        self.write(phy::EXTENDED_2::CU_PMD_TX_CTRL(), 0x02be.into())?;
        self.write(phy::TEST::TEST_PAGE_20(), 0x4320.into())?;
        self.write(phy::TEST::TEST_PAGE_24(), 0x0c00.into())?;
        self.write(phy::TEST::TEST_PAGE_9(), 0x18ca.into())?;
        self.write(phy::TEST::TEST_PAGE_5(), 0x1b20.into())?;

        // "Enable token-ring during coma-mode"
        self.modify(phy::TEST::TEST_PAGE_8(), |r| {
            r.0 |= 0x8000;
        })?;

        self.write(phy::TR::TR_18(), 0x0004.into())?;
        self.write(phy::TR::TR_17(), 0x01bd.into())?;
        self.write(phy::TR::TR_16(), 0x8fae.into())?;
        self.write(phy::TR::TR_18(), 0x000f.into())?;
        self.write(phy::TR::TR_17(), 0x000f.into())?;
        self.write(phy::TR::TR_16(), 0x8fac.into())?;
        self.write(phy::TR::TR_18(), 0x00a0.into())?;
        self.write(phy::TR::TR_17(), 0xf147.into())?;
        self.write(phy::TR::TR_16(), 0x97a0.into())?;
        self.write(phy::TR::TR_18(), 0x0005.into())?;
        self.write(phy::TR::TR_17(), 0x2f54.into())?;
        self.write(phy::TR::TR_16(), 0x8fe4.into())?;
        self.write(phy::TR::TR_18(), 0x0027.into())?;
        self.write(phy::TR::TR_17(), 0x303d.into())?;
        self.write(phy::TR::TR_16(), 0x9792.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0704.into())?;
        self.write(phy::TR::TR_16(), 0x87fe.into())?;
        self.write(phy::TR::TR_18(), 0x0006.into())?;
        self.write(phy::TR::TR_17(), 0x0150.into())?;
        self.write(phy::TR::TR_16(), 0x8fe0.into())?;
        self.write(phy::TR::TR_18(), 0x0012.into())?;
        self.write(phy::TR::TR_17(), 0xb00a.into())?;
        self.write(phy::TR::TR_16(), 0x8f82.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0d74.into())?;
        self.write(phy::TR::TR_16(), 0x8f80.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0012.into())?;
        self.write(phy::TR::TR_16(), 0x82e0.into())?;
        self.write(phy::TR::TR_18(), 0x0005.into())?;
        self.write(phy::TR::TR_17(), 0x0208.into())?;
        self.write(phy::TR::TR_16(), 0x83a2.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x9186.into())?;
        self.write(phy::TR::TR_16(), 0x83b2.into())?;
        self.write(phy::TR::TR_18(), 0x000e.into())?;
        self.write(phy::TR::TR_17(), 0x3700.into())?;
        self.write(phy::TR::TR_16(), 0x8fb0.into())?;
        self.write(phy::TR::TR_18(), 0x0004.into())?;
        self.write(phy::TR::TR_17(), 0x9f81.into())?;
        self.write(phy::TR::TR_16(), 0x9688.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0xffff.into())?;
        self.write(phy::TR::TR_16(), 0x8fd2.into())?;
        self.write(phy::TR::TR_18(), 0x0003.into())?;
        self.write(phy::TR::TR_17(), 0x9fa2.into())?;
        self.write(phy::TR::TR_16(), 0x968a.into())?;
        self.write(phy::TR::TR_18(), 0x0020.into())?;
        self.write(phy::TR::TR_17(), 0x640b.into())?;
        self.write(phy::TR::TR_16(), 0x9690.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x2220.into())?;
        self.write(phy::TR::TR_16(), 0x8258.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x2a20.into())?;
        self.write(phy::TR::TR_16(), 0x825a.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x3060.into())?;
        self.write(phy::TR::TR_16(), 0x825c.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x3fa0.into())?;
        self.write(phy::TR::TR_16(), 0x825e.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0xe0f0.into())?;
        self.write(phy::TR::TR_16(), 0x83a6.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x1489.into())?;
        self.write(phy::TR::TR_16(), 0x8f92.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x7000.into())?;
        self.write(phy::TR::TR_16(), 0x96a2.into())?;
        self.write(phy::TR::TR_18(), 0x0007.into())?;
        self.write(phy::TR::TR_17(), 0x1448.into())?;
        self.write(phy::TR::TR_16(), 0x96a6.into())?;
        self.write(phy::TR::TR_18(), 0x00ee.into())?;
        self.write(phy::TR::TR_17(), 0xffdd.into())?;
        self.write(phy::TR::TR_16(), 0x96a0.into())?;
        self.write(phy::TR::TR_18(), 0x0091.into())?;
        self.write(phy::TR::TR_17(), 0xb06c.into())?;
        self.write(phy::TR::TR_16(), 0x8fe8.into())?;
        self.write(phy::TR::TR_18(), 0x0004.into())?;
        self.write(phy::TR::TR_17(), 0x1600.into())?;
        self.write(phy::TR::TR_16(), 0x8fea.into())?;
        self.write(phy::TR::TR_18(), 0x00ee.into())?;
        self.write(phy::TR::TR_17(), 0xff00.into())?;
        self.write(phy::TR::TR_16(), 0x96b0.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x7000.into())?;
        self.write(phy::TR::TR_16(), 0x96b2.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0814.into())?;
        self.write(phy::TR::TR_16(), 0x96b4.into())?;
        self.write(phy::TR::TR_18(), 0x0068.into())?;
        self.write(phy::TR::TR_17(), 0x8980.into())?;
        self.write(phy::TR::TR_16(), 0x8f90.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0xd8f0.into())?;
        self.write(phy::TR::TR_16(), 0x83a4.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0400.into())?;
        self.write(phy::TR::TR_16(), 0x8fc0.into())?;
        self.write(phy::TR::TR_18(), 0x0050.into())?;
        self.write(phy::TR::TR_17(), 0x100f.into())?;
        self.write(phy::TR::TR_16(), 0x87fa.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0003.into())?;
        self.write(phy::TR::TR_16(), 0x8796.into())?;
        self.write(phy::TR::TR_18(), 0x00c3.into())?;
        self.write(phy::TR::TR_17(), 0xff98.into())?;
        self.write(phy::TR::TR_16(), 0x87f8.into())?;
        self.write(phy::TR::TR_18(), 0x0018.into())?;
        self.write(phy::TR::TR_17(), 0x292a.into())?;
        self.write(phy::TR::TR_16(), 0x8fa4.into())?;
        self.write(phy::TR::TR_18(), 0x00d2.into())?;
        self.write(phy::TR::TR_17(), 0xc46f.into())?;
        self.write(phy::TR::TR_16(), 0x968c.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0620.into())?;
        self.write(phy::TR::TR_16(), 0x97a2.into())?;
        self.write(phy::TR::TR_18(), 0x0013.into())?;
        self.write(phy::TR::TR_17(), 0x132f.into())?;
        self.write(phy::TR::TR_16(), 0x96a4.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0000.into())?;
        self.write(phy::TR::TR_16(), 0x96a8.into())?;
        self.write(phy::TR::TR_18(), 0x00c0.into())?;
        self.write(phy::TR::TR_17(), 0xa028.into())?;
        self.write(phy::TR::TR_16(), 0x8ffc.into())?;
        self.write(phy::TR::TR_18(), 0x0090.into())?;
        self.write(phy::TR::TR_17(), 0x1c09.into())?;
        self.write(phy::TR::TR_16(), 0x8fec.into())?;
        self.write(phy::TR::TR_18(), 0x0004.into())?;
        self.write(phy::TR::TR_17(), 0xa6a1.into())?;
        self.write(phy::TR::TR_16(), 0x8fee.into())?;
        self.write(phy::TR::TR_18(), 0x00b0.into())?;
        self.write(phy::TR::TR_17(), 0x1807.into())?;
        self.write(phy::TR::TR_16(), 0x8ffe.into())?;

        // We're not using 10BASE-TE, so this is the correct config block
        self.write(phy::TR::TR_16(), 0x028e.into())?;
        self.write(phy::TR::TR_18(), 0x0008.into())?;
        self.write(phy::TR::TR_17(), 0xa518.into())?;
        self.write(phy::TR::TR_16(), 0x8486.into())?;
        self.write(phy::TR::TR_18(), 0x006d.into())?;
        self.write(phy::TR::TR_17(), 0xc696.into())?;
        self.write(phy::TR::TR_16(), 0x8488.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0912.into())?;
        self.write(phy::TR::TR_16(), 0x848a.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0db6.into())?;
        self.write(phy::TR::TR_16(), 0x848e.into())?;
        self.write(phy::TR::TR_18(), 0x0059.into())?;
        self.write(phy::TR::TR_17(), 0x6596.into())?;
        self.write(phy::TR::TR_16(), 0x849c.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0514.into())?;
        self.write(phy::TR::TR_16(), 0x849e.into())?;
        self.write(phy::TR::TR_18(), 0x0041.into())?;
        self.write(phy::TR::TR_17(), 0x0280.into())?;
        self.write(phy::TR::TR_16(), 0x84a2.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0000.into())?;
        self.write(phy::TR::TR_16(), 0x84a4.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0000.into())?;
        self.write(phy::TR::TR_16(), 0x84a6.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0000.into())?;
        self.write(phy::TR::TR_16(), 0x84a8.into())?;
        self.write(phy::TR::TR_18(), 0x0000.into())?;
        self.write(phy::TR::TR_17(), 0x0000.into())?;
        self.write(phy::TR::TR_16(), 0x84aa.into())?;
        self.write(phy::TR::TR_18(), 0x007d.into())?;
        self.write(phy::TR::TR_17(), 0xf7dd.into())?;
        self.write(phy::TR::TR_16(), 0x84ae.into())?;
        self.write(phy::TR::TR_18(), 0x006d.into())?;
        self.write(phy::TR::TR_17(), 0x95d4.into())?;
        self.write(phy::TR::TR_16(), 0x84b0.into())?;
        self.write(phy::TR::TR_18(), 0x0049.into())?;
        self.write(phy::TR::TR_17(), 0x2410.into())?;
        self.write(phy::TR::TR_16(), 0x84b2.into())?;

        self.modify(phy::TEST::TEST_PAGE_8(), |r| {
            r.0 &= !0x8000; // Disable token-ring mode
        })?;

        self.modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS_20(), |r| {
            r.0 &= !1; // Disable broadcast write
        })?;

        //////////////////////////////////////////////////////////////////////////
        // Now we're going deep into the weeds.  This section is based on
        // `tesla_revB_8051_patch` in the SDK, which (as the name suggests), patches
        // the 8051 in the PHY.
        const FIRMWARE_START_ADDR: u16 = 0x4000;
        const PATCH_CRC_LEN: u16 = (VSC85XX_PATCH.len() + 1) as u16;
        const EXPECTED_CRC: u16 = 0x29E8;

        // This patch can only be applied to Port 0 of the PHY, so we'll check
        // the address here.
        let phy_port =
            self.read(phy::EXTENDED::EXTENDED_PHY_CONTROL_4())?.0 >> 11;
        if phy_port != 0 {
            return Err(VscError::BadPhyPatchPort(phy_port));
        }
        let crc = self.read_8051_crc(FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
        let skip_download = crc == EXPECTED_CRC;
        let patch_ok = skip_download
            && self.read(phy::GPIO::GPIO_3())?.0 == 0x3eb7
            && self.read(phy::GPIO::GPIO_4())?.0 == 0x4012
            && self.read(phy::GPIO::GPIO_12())?.0 == 0x0100
            && self.read(phy::GPIO::GPIO_0())?.0 == 0xc018;
        ringbuf_entry!(Trace::PatchState {
            patch_ok,
            skip_download
        });

        if !skip_download || !patch_ok {
            self.micro_assert_reset()?;
        }
        if !skip_download {
            self.download_patch()?;
        }
        if !patch_ok {
            // Various CPU commands to enable the patch
            self.write(phy::GPIO::GPIO_3(), 0x3eb7.into())?;
            self.write(phy::GPIO::GPIO_4(), 0x4012.into())?;
            self.write(phy::GPIO::GPIO_12(), 0x0100.into())?;
            self.write(phy::GPIO::GPIO_0(), 0xc018.into())?;
        }

        if !skip_download {
            let crc = self.read_8051_crc(FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
            if crc != EXPECTED_CRC {
                return Err(VscError::PhyPatchFailedCrc);
            }
        }

        //////////////////////////////////////////////////////////////////////////
        // `vtss_phy_pre_init_tesla_revB_1588`
        //
        // "Pass the cmd to Micro to initialize all 1588 analyzer registers to
        //  default"
        self.cmd(0x801A)?;

        Ok(())
    }

    /// Downloads a patch to the 8051 in the PHY, based on `download_8051_code`
    /// from the SDK.
    fn download_patch(&mut self) -> Result<(), VscError> {
        // "Hold 8051 in SW Reset, Enable auto incr address and patch clock,
        //  Disable the 8051 clock"
        self.write(phy::GPIO::GPIO_0(), 0x7009.into())?;

        // "write to addr 4000 = 02"
        self.write(phy::GPIO::GPIO_12(), 0x5002.into())?;

        // "write to address reg."
        self.write(phy::GPIO::GPIO_11(), 0x0.into())?;

        for &p in &VSC85XX_PATCH {
            self.write(phy::GPIO::GPIO_12(), (0x5000 | p as u16).into())?;
        }

        // "Clear internal memory access"
        self.write(phy::GPIO::GPIO_12(), 0.into())?;

        Ok(())
    }

    /// Based on `vtss_phy_micro_assert_reset`
    fn micro_assert_reset(&mut self) -> Result<(), VscError> {
        // "Pass the NOP cmd to Micro to insure that any consumptive patch exits"
        self.cmd(0x800F)?;

        // "force micro into a loop, preventing any SMI accesses"
        self.modify(phy::GPIO::GPIO_12(), |r| r.0 &= !0x0800)?;
        self.write(phy::GPIO::GPIO_9(), 0x005b.into())?;
        self.write(phy::GPIO::GPIO_10(), 0x005b.into())?;
        self.modify(phy::GPIO::GPIO_12(), |r| r.0 |= 0x0800)?;
        self.write(phy::GPIO::MICRO_PAGE(), 0x800F.into())?;

        // "Assert reset after micro is trapped in a loop (averts micro-SMI access
        //  deadlock at reset)"
        self.modify(phy::GPIO::GPIO_0(), |r| r.0 &= !0x8000)?;
        self.write(phy::GPIO::MICRO_PAGE(), 0x0000.into())?;
        self.modify(phy::GPIO::GPIO_12(), |r| r.0 &= !0x0800)?;
        Ok(())
    }

    /// Based on `vtss_phy_is_8051_crc_ok_private`
    fn read_8051_crc(&mut self, addr: u16, size: u16) -> Result<u16, VscError> {
        self.write(phy::EXTENDED::VERIPHY_CTRL_REG2(), addr.into())?;
        self.write(phy::EXTENDED::VERIPHY_CTRL_REG3(), size.into())?;

        // Start CRC calculation and wait for it to finish
        self.cmd(0x8008)?;

        let crc: u16 = self.read(phy::EXTENDED::VERIPHY_CTRL_REG2())?.into();
        ringbuf_entry!(Trace::GotCrc(crc));
        Ok(crc)
    }

    fn set_led_mode(
        &mut self,
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

/// Raw patch for 8051 microcode, from `tesla_revB_8051_patch` in the SDK
const VSC85XX_PATCH: [u8; 1655] = [
    0x46, 0x4a, 0x02, 0x43, 0x37, 0x02, 0x46, 0x26, 0x02, 0x46, 0x77, 0x02,
    0x45, 0x60, 0x02, 0x45, 0xaf, 0xed, 0xff, 0xe5, 0xfc, 0x54, 0x38, 0x64,
    0x20, 0x70, 0x08, 0x65, 0xff, 0x70, 0x04, 0xed, 0x44, 0x80, 0xff, 0x22,
    0x8f, 0x19, 0x7b, 0xbb, 0x7d, 0x0e, 0x7f, 0x04, 0x12, 0x3d, 0xd7, 0xef,
    0x4e, 0x60, 0x03, 0x02, 0x41, 0xf9, 0xe4, 0xf5, 0x1a, 0x74, 0x01, 0x7e,
    0x00, 0xa8, 0x1a, 0x08, 0x80, 0x05, 0xc3, 0x33, 0xce, 0x33, 0xce, 0xd8,
    0xf9, 0xff, 0xef, 0x55, 0x19, 0x70, 0x03, 0x02, 0x41, 0xed, 0x85, 0x1a,
    0xfb, 0x7b, 0xbb, 0xe4, 0xfd, 0xff, 0x12, 0x3d, 0xd7, 0xef, 0x4e, 0x60,
    0x03, 0x02, 0x41, 0xed, 0xe5, 0x1a, 0x54, 0x02, 0x75, 0x1d, 0x00, 0x25,
    0xe0, 0x25, 0xe0, 0xf5, 0x1c, 0xe4, 0x78, 0xc5, 0xf6, 0xd2, 0x0a, 0x12,
    0x41, 0xfa, 0x7b, 0xff, 0x7d, 0x12, 0x7f, 0x07, 0x12, 0x3d, 0xd7, 0xef,
    0x4e, 0x60, 0x03, 0x02, 0x41, 0xe7, 0xc2, 0x0a, 0x74, 0xc7, 0x25, 0x1a,
    0xf9, 0x74, 0xe7, 0x25, 0x1a, 0xf8, 0xe6, 0x27, 0xf5, 0x1b, 0xe5, 0x1d,
    0x24, 0x5b, 0x12, 0x45, 0xea, 0x12, 0x3e, 0xda, 0x7b, 0xfc, 0x7d, 0x11,
    0x7f, 0x07, 0x12, 0x3d, 0xd7, 0x78, 0xcc, 0xef, 0xf6, 0x78, 0xc1, 0xe6,
    0xfe, 0xef, 0xd3, 0x9e, 0x40, 0x06, 0x78, 0xcc, 0xe6, 0x78, 0xc1, 0xf6,
    0x12, 0x41, 0xfa, 0x7b, 0xec, 0x7d, 0x12, 0x7f, 0x07, 0x12, 0x3d, 0xd7,
    0x78, 0xcb, 0xef, 0xf6, 0xbf, 0x07, 0x06, 0x78, 0xc3, 0x76, 0x1a, 0x80,
    0x1f, 0x78, 0xc5, 0xe6, 0xff, 0x60, 0x0f, 0xc3, 0xe5, 0x1b, 0x9f, 0xff,
    0x78, 0xcb, 0xe6, 0x85, 0x1b, 0xf0, 0xa4, 0x2f, 0x80, 0x07, 0x78, 0xcb,
    0xe6, 0x85, 0x1b, 0xf0, 0xa4, 0x78, 0xc3, 0xf6, 0xe4, 0x78, 0xc2, 0xf6,
    0x78, 0xc2, 0xe6, 0xff, 0xc3, 0x08, 0x96, 0x40, 0x03, 0x02, 0x41, 0xd1,
    0xef, 0x54, 0x03, 0x60, 0x33, 0x14, 0x60, 0x46, 0x24, 0xfe, 0x60, 0x42,
    0x04, 0x70, 0x4b, 0xef, 0x24, 0x02, 0xff, 0xe4, 0x33, 0xfe, 0xef, 0x78,
    0x02, 0xce, 0xa2, 0xe7, 0x13, 0xce, 0x13, 0xd8, 0xf8, 0xff, 0xe5, 0x1d,
    0x24, 0x5c, 0xcd, 0xe5, 0x1c, 0x34, 0xf0, 0xcd, 0x2f, 0xff, 0xed, 0x3e,
    0xfe, 0x12, 0x46, 0x0d, 0x7d, 0x11, 0x80, 0x0b, 0x78, 0xc2, 0xe6, 0x70,
    0x04, 0x7d, 0x11, 0x80, 0x02, 0x7d, 0x12, 0x7f, 0x07, 0x12, 0x3e, 0x9a,
    0x8e, 0x1e, 0x8f, 0x1f, 0x80, 0x03, 0xe5, 0x1e, 0xff, 0x78, 0xc5, 0xe6,
    0x06, 0x24, 0xcd, 0xf8, 0xa6, 0x07, 0x78, 0xc2, 0x06, 0xe6, 0xb4, 0x1a,
    0x0a, 0xe5, 0x1d, 0x24, 0x5c, 0x12, 0x45, 0xea, 0x12, 0x3e, 0xda, 0x78,
    0xc5, 0xe6, 0x65, 0x1b, 0x70, 0x82, 0x75, 0xdb, 0x20, 0x75, 0xdb, 0x28,
    0x12, 0x46, 0x02, 0x12, 0x46, 0x02, 0xe5, 0x1a, 0x12, 0x45, 0xf5, 0xe5,
    0x1a, 0xc3, 0x13, 0x12, 0x45, 0xf5, 0x78, 0xc5, 0x16, 0xe6, 0x24, 0xcd,
    0xf8, 0xe6, 0xff, 0x7e, 0x08, 0x1e, 0xef, 0xa8, 0x06, 0x08, 0x80, 0x02,
    0xc3, 0x13, 0xd8, 0xfc, 0xfd, 0xc4, 0x33, 0x54, 0xe0, 0xf5, 0xdb, 0xef,
    0xa8, 0x06, 0x08, 0x80, 0x02, 0xc3, 0x13, 0xd8, 0xfc, 0xfd, 0xc4, 0x33,
    0x54, 0xe0, 0x44, 0x08, 0xf5, 0xdb, 0xee, 0x70, 0xd8, 0x78, 0xc5, 0xe6,
    0x70, 0xc8, 0x75, 0xdb, 0x10, 0x02, 0x40, 0xfd, 0x78, 0xc2, 0xe6, 0xc3,
    0x94, 0x17, 0x50, 0x0e, 0xe5, 0x1d, 0x24, 0x62, 0x12, 0x42, 0x08, 0xe5,
    0x1d, 0x24, 0x5c, 0x12, 0x42, 0x08, 0x20, 0x0a, 0x03, 0x02, 0x40, 0x76,
    0x05, 0x1a, 0xe5, 0x1a, 0xc3, 0x94, 0x04, 0x50, 0x03, 0x02, 0x40, 0x3a,
    0x22, 0xe5, 0x1d, 0x24, 0x5c, 0xff, 0xe5, 0x1c, 0x34, 0xf0, 0xfe, 0x12,
    0x46, 0x0d, 0x22, 0xff, 0xe5, 0x1c, 0x34, 0xf0, 0xfe, 0x12, 0x46, 0x0d,
    0x22, 0xe4, 0xf5, 0x19, 0x12, 0x46, 0x43, 0x20, 0xe7, 0x1e, 0x7b, 0xfe,
    0x12, 0x42, 0xf9, 0xef, 0xc4, 0x33, 0x33, 0x54, 0xc0, 0xff, 0xc0, 0x07,
    0x7b, 0x54, 0x12, 0x42, 0xf9, 0xd0, 0xe0, 0x4f, 0xff, 0x74, 0x2a, 0x25,
    0x19, 0xf8, 0xa6, 0x07, 0x12, 0x46, 0x43, 0x20, 0xe7, 0x03, 0x02, 0x42,
    0xdf, 0x54, 0x03, 0x64, 0x03, 0x70, 0x03, 0x02, 0x42, 0xcf, 0x7b, 0xcb,
    0x12, 0x43, 0x2c, 0x8f, 0xfb, 0x7b, 0x30, 0x7d, 0x03, 0xe4, 0xff, 0x12,
    0x3d, 0xd7, 0xc3, 0xef, 0x94, 0x02, 0xee, 0x94, 0x00, 0x50, 0x2a, 0x12,
    0x42, 0xec, 0xef, 0x4e, 0x70, 0x23, 0x12, 0x43, 0x04, 0x60, 0x0a, 0x12,
    0x43, 0x12, 0x70, 0x0c, 0x12, 0x43, 0x1f, 0x70, 0x07, 0x12, 0x46, 0x39,
    0x7b, 0x03, 0x80, 0x07, 0x12, 0x46, 0x39, 0x12, 0x46, 0x43, 0xfb, 0x7a,
    0x00, 0x7d, 0x54, 0x80, 0x3e, 0x12, 0x42, 0xec, 0xef, 0x4e, 0x70, 0x24,
    0x12, 0x43, 0x04, 0x60, 0x0a, 0x12, 0x43, 0x12, 0x70, 0x0f, 0x12, 0x43,
    0x1f, 0x70, 0x0a, 0x12, 0x46, 0x39, 0xe4, 0xfb, 0xfa, 0x7d, 0xee, 0x80,
    0x1e, 0x12, 0x46, 0x39, 0x7b, 0x01, 0x7a, 0x00, 0x7d, 0xee, 0x80, 0x13,
    0x12, 0x46, 0x39, 0x12, 0x46, 0x43, 0x54, 0x40, 0xfe, 0xc4, 0x13, 0x13,
    0x54, 0x03, 0xfb, 0x7a, 0x00, 0x7d, 0xee, 0x12, 0x38, 0xbd, 0x7b, 0xff,
    0x12, 0x43, 0x2c, 0xef, 0x4e, 0x70, 0x07, 0x74, 0x2a, 0x25, 0x19, 0xf8,
    0xe4, 0xf6, 0x05, 0x19, 0xe5, 0x19, 0xc3, 0x94, 0x02, 0x50, 0x03, 0x02,
    0x42, 0x15, 0x22, 0xe5, 0x19, 0x24, 0x17, 0xfd, 0x7b, 0x20, 0x7f, 0x04,
    0x12, 0x3d, 0xd7, 0x22, 0xe5, 0x19, 0x24, 0x17, 0xfd, 0x7f, 0x04, 0x12,
    0x3d, 0xd7, 0x22, 0x7b, 0x22, 0x7d, 0x18, 0x7f, 0x06, 0x12, 0x3d, 0xd7,
    0xef, 0x64, 0x01, 0x4e, 0x22, 0x7d, 0x1c, 0xe4, 0xff, 0x12, 0x3e, 0x9a,
    0xef, 0x54, 0x1b, 0x64, 0x0a, 0x22, 0x7b, 0xcc, 0x7d, 0x10, 0xff, 0x12,
    0x3d, 0xd7, 0xef, 0x64, 0x01, 0x4e, 0x22, 0xe5, 0x19, 0x24, 0x17, 0xfd,
    0x7f, 0x04, 0x12, 0x3d, 0xd7, 0x22, 0xd2, 0x08, 0x75, 0xfb, 0x03, 0xab,
    0x7e, 0xaa, 0x7d, 0x7d, 0x19, 0x7f, 0x03, 0x12, 0x3e, 0xda, 0xe5, 0x7e,
    0x54, 0x0f, 0x24, 0xf3, 0x60, 0x03, 0x02, 0x43, 0xe9, 0x12, 0x46, 0x5a,
    0x12, 0x46, 0x61, 0xd8, 0xfb, 0xff, 0x20, 0xe2, 0x35, 0x13, 0x92, 0x0c,
    0xef, 0xa2, 0xe1, 0x92, 0x0b, 0x30, 0x0c, 0x2a, 0xe4, 0xf5, 0x10, 0x7b,
    0xfe, 0x12, 0x43, 0xff, 0xef, 0xc4, 0x33, 0x33, 0x54, 0xc0, 0xff, 0xc0,
    0x07, 0x7b, 0x54, 0x12, 0x43, 0xff, 0xd0, 0xe0, 0x4f, 0xff, 0x74, 0x2a,
    0x25, 0x10, 0xf8, 0xa6, 0x07, 0x05, 0x10, 0xe5, 0x10, 0xc3, 0x94, 0x02,
    0x40, 0xd9, 0x12, 0x46, 0x5a, 0x12, 0x46, 0x61, 0xd8, 0xfb, 0x54, 0x05,
    0x64, 0x04, 0x70, 0x27, 0x78, 0xc4, 0xe6, 0x78, 0xc6, 0xf6, 0xe5, 0x7d,
    0xff, 0x33, 0x95, 0xe0, 0xef, 0x54, 0x0f, 0x78, 0xc4, 0xf6, 0x12, 0x44,
    0x0a, 0x20, 0x0c, 0x0c, 0x12, 0x46, 0x5a, 0x12, 0x46, 0x61, 0xd8, 0xfb,
    0x13, 0x92, 0x0d, 0x22, 0xc2, 0x0d, 0x22, 0x12, 0x46, 0x5a, 0x12, 0x46,
    0x61, 0xd8, 0xfb, 0x54, 0x05, 0x64, 0x05, 0x70, 0x1e, 0x78, 0xc4, 0x7d,
    0xb8, 0x12, 0x43, 0xf5, 0x78, 0xc1, 0x7d, 0x74, 0x12, 0x43, 0xf5, 0xe4,
    0x78, 0xc1, 0xf6, 0x22, 0x7b, 0x01, 0x7a, 0x00, 0x7d, 0xee, 0x7f, 0x92,
    0x12, 0x38, 0xbd, 0x22, 0xe6, 0xfb, 0x7a, 0x00, 0x7f, 0x92, 0x12, 0x38,
    0xbd, 0x22, 0xe5, 0x10, 0x24, 0x17, 0xfd, 0x7f, 0x04, 0x12, 0x3d, 0xd7,
    0x22, 0x78, 0xc1, 0xe6, 0xfb, 0x7a, 0x00, 0x7d, 0x74, 0x7f, 0x92, 0x12,
    0x38, 0xbd, 0xe4, 0x78, 0xc1, 0xf6, 0xf5, 0x11, 0x74, 0x01, 0x7e, 0x00,
    0xa8, 0x11, 0x08, 0x80, 0x05, 0xc3, 0x33, 0xce, 0x33, 0xce, 0xd8, 0xf9,
    0xff, 0x78, 0xc4, 0xe6, 0xfd, 0xef, 0x5d, 0x60, 0x44, 0x85, 0x11, 0xfb,
    0xe5, 0x11, 0x54, 0x02, 0x25, 0xe0, 0x25, 0xe0, 0xfe, 0xe4, 0x24, 0x5b,
    0xfb, 0xee, 0x12, 0x45, 0xed, 0x12, 0x3e, 0xda, 0x7b, 0x40, 0x7d, 0x11,
    0x7f, 0x07, 0x12, 0x3d, 0xd7, 0x74, 0xc7, 0x25, 0x11, 0xf8, 0xa6, 0x07,
    0x7b, 0x11, 0x7d, 0x12, 0x7f, 0x07, 0x12, 0x3d, 0xd7, 0xef, 0x4e, 0x60,
    0x09, 0x74, 0xe7, 0x25, 0x11, 0xf8, 0x76, 0x04, 0x80, 0x07, 0x74, 0xe7,
    0x25, 0x11, 0xf8, 0x76, 0x0a, 0x05, 0x11, 0xe5, 0x11, 0xc3, 0x94, 0x04,
    0x40, 0x9a, 0x78, 0xc6, 0xe6, 0x70, 0x15, 0x78, 0xc4, 0xe6, 0x60, 0x10,
    0x75, 0xd9, 0x38, 0x75, 0xdb, 0x10, 0x7d, 0xfe, 0x12, 0x44, 0xb8, 0x7d,
    0x76, 0x12, 0x44, 0xb8, 0x79, 0xc6, 0xe7, 0x78, 0xc4, 0x66, 0xff, 0x60,
    0x03, 0x12, 0x40, 0x25, 0x78, 0xc4, 0xe6, 0x70, 0x09, 0xfb, 0xfa, 0x7d,
    0xfe, 0x7f, 0x8e, 0x12, 0x38, 0xbd, 0x22, 0x7b, 0x01, 0x7a, 0x00, 0x7f,
    0x8e, 0x12, 0x38, 0xbd, 0x22, 0xe4, 0xf5, 0xfb, 0x7d, 0x1c, 0xe4, 0xff,
    0x12, 0x3e, 0x9a, 0xad, 0x07, 0xac, 0x06, 0xec, 0x54, 0xc0, 0xff, 0xed,
    0x54, 0x3f, 0x4f, 0xf5, 0x20, 0x30, 0x06, 0x2c, 0x30, 0x01, 0x08, 0xa2,
    0x04, 0x72, 0x03, 0x92, 0x07, 0x80, 0x21, 0x30, 0x04, 0x06, 0x7b, 0xcc,
    0x7d, 0x11, 0x80, 0x0d, 0x30, 0x03, 0x06, 0x7b, 0xcc, 0x7d, 0x10, 0x80,
    0x04, 0x7b, 0x66, 0x7d, 0x16, 0xe4, 0xff, 0x12, 0x3d, 0xd7, 0xee, 0x4f,
    0x24, 0xff, 0x92, 0x07, 0xaf, 0xfb, 0x74, 0x26, 0x2f, 0xf8, 0xe6, 0xff,
    0xa6, 0x20, 0x20, 0x07, 0x39, 0x8f, 0x20, 0x30, 0x07, 0x34, 0x30, 0x00,
    0x31, 0x20, 0x04, 0x2e, 0x20, 0x03, 0x2b, 0xe4, 0xf5, 0xff, 0x75, 0xfc,
    0xc2, 0xe5, 0xfc, 0x30, 0xe0, 0xfb, 0xaf, 0xfe, 0xef, 0x20, 0xe3, 0x1a,
    0xae, 0xfd, 0x44, 0x08, 0xf5, 0xfe, 0x75, 0xfc, 0x80, 0xe5, 0xfc, 0x30,
    0xe0, 0xfb, 0x8f, 0xfe, 0x8e, 0xfd, 0x75, 0xfc, 0x80, 0xe5, 0xfc, 0x30,
    0xe0, 0xfb, 0x05, 0xfb, 0xaf, 0xfb, 0xef, 0xc3, 0x94, 0x04, 0x50, 0x03,
    0x02, 0x44, 0xc5, 0xe4, 0xf5, 0xfb, 0x22, 0xe5, 0x7e, 0x54, 0x0f, 0x64,
    0x01, 0x70, 0x23, 0xe5, 0x7e, 0x30, 0xe4, 0x1e, 0x90, 0x47, 0xd0, 0xe0,
    0x44, 0x02, 0xf0, 0x54, 0xfb, 0xf0, 0x90, 0x47, 0xd4, 0xe0, 0x44, 0x04,
    0xf0, 0x7b, 0x03, 0x7d, 0x5b, 0x7f, 0x5d, 0x12, 0x36, 0x29, 0x7b, 0x0e,
    0x80, 0x1c, 0x90, 0x47, 0xd0, 0xe0, 0x54, 0xfd, 0xf0, 0x44, 0x04, 0xf0,
    0x90, 0x47, 0xd4, 0xe0, 0x54, 0xfb, 0xf0, 0x7b, 0x02, 0x7d, 0x5b, 0x7f,
    0x5d, 0x12, 0x36, 0x29, 0x7b, 0x06, 0x7d, 0x60, 0x7f, 0x63, 0x12, 0x36,
    0x29, 0x22, 0xe5, 0x7e, 0x30, 0xe5, 0x35, 0x30, 0xe4, 0x0b, 0x7b, 0x02,
    0x7d, 0x33, 0x7f, 0x35, 0x12, 0x36, 0x29, 0x80, 0x10, 0x7b, 0x01, 0x7d,
    0x33, 0x7f, 0x35, 0x12, 0x36, 0x29, 0x90, 0x47, 0xd2, 0xe0, 0x44, 0x04,
    0xf0, 0x90, 0x47, 0xd2, 0xe0, 0x54, 0xf7, 0xf0, 0x90, 0x47, 0xd1, 0xe0,
    0x44, 0x10, 0xf0, 0x7b, 0x05, 0x7d, 0x84, 0x7f, 0x86, 0x12, 0x36, 0x29,
    0x22, 0xfb, 0xe5, 0x1c, 0x34, 0xf0, 0xfa, 0x7d, 0x10, 0x7f, 0x07, 0x22,
    0x54, 0x01, 0xc4, 0x33, 0x54, 0xe0, 0xf5, 0xdb, 0x44, 0x08, 0xf5, 0xdb,
    0x22, 0xf5, 0xdb, 0x75, 0xdb, 0x08, 0xf5, 0xdb, 0x75, 0xdb, 0x08, 0x22,
    0xab, 0x07, 0xaa, 0x06, 0x7d, 0x10, 0x7f, 0x07, 0x12, 0x3e, 0xda, 0x7b,
    0xff, 0x7d, 0x10, 0x7f, 0x07, 0x12, 0x3d, 0xd7, 0xef, 0x4e, 0x60, 0xf3,
    0x22, 0x12, 0x44, 0xc2, 0x30, 0x0c, 0x03, 0x12, 0x42, 0x12, 0x78, 0xc4,
    0xe6, 0xff, 0x60, 0x03, 0x12, 0x40, 0x25, 0x22, 0xe5, 0x19, 0x24, 0x17,
    0x54, 0x1f, 0x44, 0x80, 0xff, 0x22, 0x74, 0x2a, 0x25, 0x19, 0xf8, 0xe6,
    0x22, 0x12, 0x46, 0x72, 0x12, 0x46, 0x68, 0x90, 0x47, 0xfa, 0xe0, 0x54,
    0xf8, 0x44, 0x02, 0xf0, 0x22, 0xe5, 0x7e, 0xae, 0x7d, 0x78, 0x04, 0x22,
    0xce, 0xa2, 0xe7, 0x13, 0xce, 0x13, 0x22, 0xe4, 0x78, 0xc4, 0xf6, 0xc2,
    0x0d, 0x78, 0xc1, 0xf6, 0x22, 0xc2, 0x0c, 0xc2, 0x0b, 0x22, 0x22,
];
