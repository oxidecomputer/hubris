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

////////////////////////////////////////////////////////////////////////////////

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

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Vsc8522Init(u8),
    Vsc8552Patch(u8),
    Vsc8562Patch(u8),
    Vsc8552Init(u8),
    Vsc8562Init(u8),
    Vsc8504Init(u8),
    PatchState { patch_ok: bool, skip_download: bool },
    GotCrc(u16),
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

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

fn set_led_mode<P: PhyRw>(
    v: &mut Phy<P>,
    led: LED,
    mode: LEDMode,
) -> Result<(), VscError> {
    v.modify(phy::STANDARD::LED_MODE_SELECT(), |r| {
        let shift_amount = led as u8 * 4;
        r.0 = (r.0 & !(0xf << shift_amount)) | ((mode as u16) << shift_amount);
    })
}

////////////////////////////////////////////////////////////////////////////////

// These IDs are (id1 << 16) | id2, meaning they also capture device revision
// number.  This matters, because the patches are device-revision specific.
const VSC8504_ID: u32 = 0x704c2;
const VSC8522_ID: u32 = 0x706f3;
const VSC8552_ID: u32 = 0x704e2;
const VSC8562_ID: u32 = 0x7071b;

pub fn read_id<P: PhyRw>(v: &mut Phy<P>) -> Result<u32, VscError> {
    let id1 = v.read(phy::STANDARD::IDENTIFIER_1())?.0;
    let id2 = v.read(phy::STANDARD::IDENTIFIER_2())?.0;
    Ok((u32::from(id1) << 16) | u32::from(id2))
}

pub fn software_reset<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    v.modify(phy::STANDARD::MODE_CONTROL(), |r| {
        r.set_sw_reset(1);
    })?;
    v.wait_timeout(phy::STANDARD::MODE_CONTROL(), |r| r.sw_reset() != 1)
}

/// Initializes a VSC8522 PHY using QSGMII.
/// This is the PHY on the VSC7448 dev kit.
pub fn init_vsc8522_phy<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8522Init(v.port));

    let id = read_id(v)?;
    if id != VSC8522_ID {
        return Err(VscError::BadPhyId(id));
    }

    // Disable COMA MODE, which keeps the chip holding itself in reset
    v.modify(phy::GPIO::GPIO_CONTROL_2(), |g| {
        g.set_coma_mode_output_enable(0)
    })?;

    // Configure the PHY in QSGMII + 12 port mode
    cmd(v, 0x80A0)?;
    Ok(())
}

/// Initializes a VSC8504 PHY using QSGMII, based on the "Configuration"
/// guide in the datasheet (section 3.19).  This should be called _after_
/// the PHY is reset (i.e. the reset pin is toggled and then the caller
/// waits for 120 ms).  The caller is also responsible for handling the
/// `COMA_MODE` pin.
pub fn init_vsc8504_phy<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8504Init(v.port));

    let id = read_id(v)?;
    if id != VSC8504_ID {
        return Err(VscError::BadPhyId(id));
    }

    let rev = v.read(phy::GPIO::EXTENDED_REVISION())?;
    if rev.tesla_e() != 1 {
        return Err(VscError::BadPhyRev);
    }

    patch_tesla(v)?;

    // Configure MAC in QSGMII mode
    v.modify(phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
        r.0 = (r.0 & !(0b11 << 14)) | (0b01 << 14)
    })?;

    // Enable 4 port MAC QSGMII
    cmd(v, 0x80E0)?;

    // The PHY is already configured for copper in register 23
    // XXX: I don't think this is correct

    // Now, we reset the PHY to put those settings into effect
    software_reset(v)
}

/// Checks the chip ID of a VSC8552 or VSC8562 PHY, then applies a patch to the
/// built-in 8051 processor based on the MESA SDK.  This must only be called on
/// port 0 in the PHY; otherwise it will return an error
///
/// This should be called _after_ the PHY is reset
/// (i.e. the reset pin is toggled and then the caller waits for 120 ms).
/// The caller is also responsible for handling the `COMA_MODE` pin.
pub fn patch_vsc85x2_phy<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    match read_id(v)? {
        VSC8552_ID => {
            let rev = v.read(phy::GPIO::EXTENDED_REVISION())?;
            if rev.tesla_e() == 1 {
                ringbuf_entry!(Trace::Vsc8552Patch(v.port));
                patch_tesla(v)
            } else {
                Err(VscError::BadPhyRev)
            }
        }
        VSC8562_ID => {
            ringbuf_entry!(Trace::Vsc8562Patch(v.port));
            patch_viper(v)
        }
        i => Err(VscError::UnknownPhyId(i)),
    }
}

/// Initializes either a VSC8552 or VSC8562 PHY, configuring it to use 2x SGMII
/// to 100BASE-FX SFP fiber). This should be called _after_ [patch_vsc85x2_phy],
/// and has the same caveats w.r.t. the reset and COMA_MODE pins.
pub fn init_vsc85x2_phy<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    match read_id(v)? {
        VSC8552_ID => init_vsc8552_phy(v),
        VSC8562_ID => init_vsc8562_phy(v),
        i => Err(VscError::UnknownPhyId(i)),
    }
}

/// Initializes a VSC8552 PHY using SGMII based on section 3.1.2 (2x SGMII
/// to 100BASE-FX SFP Fiber). This should be called _after_ [patch_tesla],
/// and has the same caveats w.r.t. the reset and COMA_MODE pins.
pub fn init_vsc8552_phy<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8552Init(v.port));

    v.modify(phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
        // MAC configuration = SGMII
        r.0 &= !(0b11 << 14)
    })?;

    // Enable 2 port MAC SGMII, then wait for the command to finish
    cmd(v, 0x80F0)?;

    v.modify(phy::STANDARD::EXTENDED_PHY_CONTROL(), |r| {
        // SGMII MAC interface mode
        r.set_mac_interface_mode(0);
        // 100BASE-FX fiber/SFP on the fiber media pins only
        r.set_media_operating_mode(0b11);
    })?;

    // Enable 2 ports Media 100BASE-FX
    cmd(v, 0x8FD1)?;

    // Configure LEDs.
    set_led_mode(v, LED::LED0, LEDMode::ForcedOff)?;
    set_led_mode(v, LED::LED1, LEDMode::Link100BaseFXLink1000BaseXActivity)?;
    set_led_mode(v, LED::LED2, LEDMode::Activity)?;
    set_led_mode(v, LED::LED3, LEDMode::Fiber100Fiber1000Activity)?;

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

    // Now, we reset the PHY to put those settings into effect
    software_reset(v)
}

/// Initializes a VSC8562 PHY using SGMII based on section 3.1.2.1 (2x SGMII
/// to 100BASE-FX SFP Fiber). This should be called _after_ [patch_viper],
/// and has the same caveats w.r.t. the reset and COMA_MODE pins.
pub fn init_vsc8562_phy<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8562Init(v.port));

    v.modify(phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
        // MAC configuration = SGMII
        r.0 &= !(0b11 << 14)
    })?;

    // Enable 2 port MAC SGMII, then wait for the command to finish
    cmd(v, 0x80F0)?;

    // 100BASE-FX on all PHYs
    cmd(v, 0x8FD1)?;

    v.modify(phy::STANDARD::EXTENDED_PHY_CONTROL(), |r| {
        // SGMII MAC interface mode
        r.set_mac_interface_mode(0);
        // 100BASE-FX fiber/SFP on the fiber media pins only
        r.set_media_operating_mode(0b11);
    })?;

    // Now, we reset the PHY to put those settings into effect
    software_reset(v)

    // TODO: "Apply Enhanced SerDes patch from PHY_API"
    // I think this is `vtss_phy_chk_serdes_patch_init_private` and
    // `vtss_phy_sd6g_patch_private`?
}

/// Represents a VSC8552 or VSC8562 PHY.  `base_port` is the PHY address of
/// the chip's port 0; since this is a two-port PHY, we can address either
/// `base_port` or `base_port + 1` given a suitable `PhyRw`.
pub struct Vsc85x2 {
    base_port: u8,
}

impl Vsc85x2 {
    pub fn new(base_port: u8) -> Self {
        Self { base_port }
    }

    /// Returns a handle to address the specified port, which must be either 0
    /// or 1; this function offsets by the chip's port offset, which is set
    /// by resistor strapping.
    pub fn phy<'a, P: PhyRw>(&self, port: u8, rw: &'a mut P) -> Phy<'a, P> {
        assert!(port < 2);
        Phy {
            port: self.base_port + port,
            rw,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

/// The VSC85xx family supports sending commands to the system by writing to
/// register 19G.  This helper function sends a command then waits for it
/// to finish, return [VscError::PhyInitTimeout] if it fails (or another
/// [VscError] if communication to the PHY doesn't work)
fn cmd<P: PhyRw>(v: &mut Phy<P>, command: u16) -> Result<(), VscError> {
    v.write(phy::GPIO::MICRO_PAGE(), command.into())?;
    v.wait_timeout(phy::GPIO::MICRO_PAGE(), |r| r.0 & 0x8000 == 0)?;
    Ok(())
}

/// Applies a patch to the 8051 microcode inside the PHY, based on
/// `vtss_phy_pre_init_seq_viper` in the SDK, which calls
/// `vtss_phy_pre_init_seq_viper_rev_b`
fn patch_viper<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    v.modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
        *r = (u16::from(*r) | 1).into()
    })?;
    v.modify(phy::STANDARD::BYPASS_CONTROL(), |r| {
        *r = (u16::from(*r) | 8).into()
    })?;
    v.write(
        phy::EXTENDED_3::MEDIA_SERDES_TX_CRC_ERROR_COUNTER(),
        0x2000.into(),
    )?;
    v.write(phy::TEST::TEST_PAGE_5(), 0x1f20.into())?;
    v.modify(phy::TEST::TEST_PAGE_8(), |r| r.0 |= 0x8000)?;
    v.write(phy::TR::TR_16(), 0xafa4.into())?;
    v.modify(phy::TR::TR_18(), |r| r.0 = (r.0 & !0x7f) | 0x19)?;

    v.write(phy::TR::TR_16(), 0x8fa4.into())?;
    v.write(phy::TR::TR_18(), 0x0050.into())?;
    v.write(phy::TR::TR_17(), 0x100f.into())?;
    v.write(phy::TR::TR_16(), 0x87fa.into())?;
    v.write(phy::TR::TR_18(), 0x0004.into())?;
    v.write(phy::TR::TR_17(), 0x9f81.into())?;
    v.write(phy::TR::TR_16(), 0x9688.into())?;

    // "Init script updates from James Bz#22267"
    v.write(phy::TR::TR_18(), 0x0068.into())?;
    v.write(phy::TR::TR_17(), 0x8980.into())?;
    v.write(phy::TR::TR_16(), 0x8f90.into())?;

    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0xd8f0.into())?;
    v.write(phy::TR::TR_16(), 0x83a4.into())?;

    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0400.into())?;
    v.write(phy::TR::TR_16(), 0x8fc0.into())?;

    // "EEE updates from James Bz#22267"
    v.write(phy::TR::TR_18(), 0x0012.into())?;
    v.write(phy::TR::TR_17(), 0xb002.into())?;
    v.write(phy::TR::TR_16(), 0x8f82.into())?;

    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0004.into())?;
    v.write(phy::TR::TR_16(), 0x9686.into())?;

    v.write(phy::TR::TR_18(), 0x00d2.into())?;
    v.write(phy::TR::TR_17(), 0xc46f.into())?;
    v.write(phy::TR::TR_16(), 0x968c.into())?;

    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0620.into())?;
    v.write(phy::TR::TR_16(), 0x97a2.into())?;

    v.write(phy::TR::TR_18(), 0x00ee.into())?;
    v.write(phy::TR::TR_17(), 0xffdd.into())?;
    v.write(phy::TR::TR_16(), 0x96a0.into())?;

    v.write(phy::TR::TR_18(), 0x0007.into())?;
    v.write(phy::TR::TR_17(), 0x1448.into())?;
    v.write(phy::TR::TR_16(), 0x96a6.into())?;

    v.write(phy::TR::TR_18(), 0x0013.into())?;
    v.write(phy::TR::TR_17(), 0x132f.into())?;
    v.write(phy::TR::TR_16(), 0x96a4.into())?;

    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0000.into())?;
    v.write(phy::TR::TR_16(), 0x96a8.into())?;

    v.write(phy::TR::TR_18(), 0x00c0.into())?;
    v.write(phy::TR::TR_17(), 0xa028.into())?;
    v.write(phy::TR::TR_16(), 0x8ffc.into())?;

    v.write(phy::TR::TR_18(), 0x0091.into())?;
    v.write(phy::TR::TR_17(), 0xb06c.into())?;
    v.write(phy::TR::TR_16(), 0x8fe8.into())?;

    v.write(phy::TR::TR_18(), 0x0004.into())?;
    v.write(phy::TR::TR_17(), 0x1600.into())?;
    v.write(phy::TR::TR_16(), 0x8fea.into())?;

    v.write(phy::TR::TR_18(), 0x00ff.into())?;
    v.write(phy::TR::TR_17(), 0xfaff.into())?;
    v.write(phy::TR::TR_16(), 0x8f80.into())?;

    v.write(phy::TR::TR_18(), 0x0090.into())?;
    v.write(phy::TR::TR_17(), 0x1809.into())?;
    v.write(phy::TR::TR_16(), 0x8fec.into())?;

    v.write(phy::TR::TR_18(), 0x00b0.into())?;
    v.write(phy::TR::TR_17(), 0x1007.into())?;
    v.write(phy::TR::TR_16(), 0x8ffe.into())?;

    v.write(phy::TR::TR_18(), 0x00ee.into())?;
    v.write(phy::TR::TR_17(), 0xff00.into())?;
    v.write(phy::TR::TR_16(), 0x96b0.into())?;

    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x7000.into())?;
    v.write(phy::TR::TR_16(), 0x96b2.into())?;

    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0814.into())?;
    v.write(phy::TR::TR_16(), 0x96b4.into())?;

    // We aren't using 10Base-TE, so this is correct config block
    v.write(phy::EXTENDED_2::CU_PMD_TX_CTRL(), 0x028e.into())?;
    v.write(phy::TR::TR_18(), 0x0008.into())?;
    v.write(phy::TR::TR_17(), 0xa518.into())?;
    v.write(phy::TR::TR_16(), 0x8486.into())?;
    v.write(phy::TR::TR_18(), 0x006d.into())?;
    v.write(phy::TR::TR_17(), 0xc696.into())?;
    v.write(phy::TR::TR_16(), 0x8488.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0912.into())?;
    v.write(phy::TR::TR_16(), 0x848a.into())?;

    v.modify(phy::TEST::TEST_PAGE_8(), |r| {
        r.0 &= !0x8000;
    })?;
    v.modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
        *r = (u16::from(*r) & !1).into();
    })?;

    //////////////////////////////////////////////////////////////////////
    // Now, we do the fun part of patching the 8051 PHY, based on
    // `viper_revB_8051_patch` in the SDK

    const FIRMWARE_START_ADDR: u16 = 0xE800;
    const PATCH_CRC_LEN: u16 = (VIPER_PATCH.len() + 1) as u16;
    const EXPECTED_CRC: u16 = 0xFB48;
    // This patch can only be applied to Port 0 of the PHY, so we'll check
    // the address here.
    let phy_port = v.read(phy::EXTENDED::EXTENDED_PHY_CONTROL_4())?.0 >> 11;
    if phy_port != 0 {
        return Err(VscError::BadPhyPatchPort(phy_port));
    }

    let crc = read_8051_crc(v, FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
    if crc == EXPECTED_CRC {
        return Ok(());
    }

    download_patch(v, &VIPER_PATCH)?;
    // These writes only happen if vtss_state->syn_calling_private is
    // false, which seems like the default state?
    v.write(phy::GPIO::GPIO_0(), 0x4018.into())?;
    v.write(phy::GPIO::GPIO_0(), 0xc018.into())?;

    // Reread the CRC to make sure the download succeeded
    let crc = read_8051_crc(v, FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
    if crc != EXPECTED_CRC {
        return Err(VscError::PhyPatchFailedCrc);
    }

    micro_assert_reset(v)?;

    // "Clear all patches"
    v.write(phy::GPIO::GPIO_12(), 0.into())?;

    // "Enable 8051 clock; set patch present; disable PRAM clock override
    //  and addr. auto-incr; operate at 125 MHz"
    v.write(phy::GPIO::GPIO_0(), 0x4098.into())?;

    // "Release 8051 SW Reset"
    v.write(phy::GPIO::GPIO_0(), 0xc098.into())?;

    // I'm not sure if these writes to GPIO_0 are superfluous, because we
    // also wrote to it above right after download_patch was called.
    Ok(())
}

/// Applies a patch to the 8051 microcode inside the PHY, based on
/// `vtss_phy_pre_init_seq_tesla_rev_e` in the SDK
fn patch_tesla<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    // Enable broadcast flag to configure all ports simultaneously
    v.modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
        *r = (u16::from(*r) | 1).into();
    })?;

    v.write(phy::STANDARD::EXTENDED_PHY_CONTROL_2(), 0x0040.into())?;
    v.write(phy::EXTENDED_2::CU_PMD_TX_CTRL(), 0x02be.into())?;
    v.write(phy::TEST::TEST_PAGE_20(), 0x4320.into())?;
    v.write(phy::TEST::TEST_PAGE_24(), 0x0c00.into())?;
    v.write(phy::TEST::TEST_PAGE_9(), 0x18ca.into())?;
    v.write(phy::TEST::TEST_PAGE_5(), 0x1b20.into())?;

    // "Enable token-ring during coma-mode"
    v.modify(phy::TEST::TEST_PAGE_8(), |r| {
        r.0 |= 0x8000;
    })?;

    v.write(phy::TR::TR_18(), 0x0004.into())?;
    v.write(phy::TR::TR_17(), 0x01bd.into())?;
    v.write(phy::TR::TR_16(), 0x8fae.into())?;
    v.write(phy::TR::TR_18(), 0x000f.into())?;
    v.write(phy::TR::TR_17(), 0x000f.into())?;
    v.write(phy::TR::TR_16(), 0x8fac.into())?;
    v.write(phy::TR::TR_18(), 0x00a0.into())?;
    v.write(phy::TR::TR_17(), 0xf147.into())?;
    v.write(phy::TR::TR_16(), 0x97a0.into())?;
    v.write(phy::TR::TR_18(), 0x0005.into())?;
    v.write(phy::TR::TR_17(), 0x2f54.into())?;
    v.write(phy::TR::TR_16(), 0x8fe4.into())?;
    v.write(phy::TR::TR_18(), 0x0027.into())?;
    v.write(phy::TR::TR_17(), 0x303d.into())?;
    v.write(phy::TR::TR_16(), 0x9792.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0704.into())?;
    v.write(phy::TR::TR_16(), 0x87fe.into())?;
    v.write(phy::TR::TR_18(), 0x0006.into())?;
    v.write(phy::TR::TR_17(), 0x0150.into())?;
    v.write(phy::TR::TR_16(), 0x8fe0.into())?;
    v.write(phy::TR::TR_18(), 0x0012.into())?;
    v.write(phy::TR::TR_17(), 0xb00a.into())?;
    v.write(phy::TR::TR_16(), 0x8f82.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0d74.into())?;
    v.write(phy::TR::TR_16(), 0x8f80.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0012.into())?;
    v.write(phy::TR::TR_16(), 0x82e0.into())?;
    v.write(phy::TR::TR_18(), 0x0005.into())?;
    v.write(phy::TR::TR_17(), 0x0208.into())?;
    v.write(phy::TR::TR_16(), 0x83a2.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x9186.into())?;
    v.write(phy::TR::TR_16(), 0x83b2.into())?;
    v.write(phy::TR::TR_18(), 0x000e.into())?;
    v.write(phy::TR::TR_17(), 0x3700.into())?;
    v.write(phy::TR::TR_16(), 0x8fb0.into())?;
    v.write(phy::TR::TR_18(), 0x0004.into())?;
    v.write(phy::TR::TR_17(), 0x9f81.into())?;
    v.write(phy::TR::TR_16(), 0x9688.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0xffff.into())?;
    v.write(phy::TR::TR_16(), 0x8fd2.into())?;
    v.write(phy::TR::TR_18(), 0x0003.into())?;
    v.write(phy::TR::TR_17(), 0x9fa2.into())?;
    v.write(phy::TR::TR_16(), 0x968a.into())?;
    v.write(phy::TR::TR_18(), 0x0020.into())?;
    v.write(phy::TR::TR_17(), 0x640b.into())?;
    v.write(phy::TR::TR_16(), 0x9690.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x2220.into())?;
    v.write(phy::TR::TR_16(), 0x8258.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x2a20.into())?;
    v.write(phy::TR::TR_16(), 0x825a.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x3060.into())?;
    v.write(phy::TR::TR_16(), 0x825c.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x3fa0.into())?;
    v.write(phy::TR::TR_16(), 0x825e.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0xe0f0.into())?;
    v.write(phy::TR::TR_16(), 0x83a6.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x1489.into())?;
    v.write(phy::TR::TR_16(), 0x8f92.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x7000.into())?;
    v.write(phy::TR::TR_16(), 0x96a2.into())?;
    v.write(phy::TR::TR_18(), 0x0007.into())?;
    v.write(phy::TR::TR_17(), 0x1448.into())?;
    v.write(phy::TR::TR_16(), 0x96a6.into())?;
    v.write(phy::TR::TR_18(), 0x00ee.into())?;
    v.write(phy::TR::TR_17(), 0xffdd.into())?;
    v.write(phy::TR::TR_16(), 0x96a0.into())?;
    v.write(phy::TR::TR_18(), 0x0091.into())?;
    v.write(phy::TR::TR_17(), 0xb06c.into())?;
    v.write(phy::TR::TR_16(), 0x8fe8.into())?;
    v.write(phy::TR::TR_18(), 0x0004.into())?;
    v.write(phy::TR::TR_17(), 0x1600.into())?;
    v.write(phy::TR::TR_16(), 0x8fea.into())?;
    v.write(phy::TR::TR_18(), 0x00ee.into())?;
    v.write(phy::TR::TR_17(), 0xff00.into())?;
    v.write(phy::TR::TR_16(), 0x96b0.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x7000.into())?;
    v.write(phy::TR::TR_16(), 0x96b2.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0814.into())?;
    v.write(phy::TR::TR_16(), 0x96b4.into())?;
    v.write(phy::TR::TR_18(), 0x0068.into())?;
    v.write(phy::TR::TR_17(), 0x8980.into())?;
    v.write(phy::TR::TR_16(), 0x8f90.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0xd8f0.into())?;
    v.write(phy::TR::TR_16(), 0x83a4.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0400.into())?;
    v.write(phy::TR::TR_16(), 0x8fc0.into())?;
    v.write(phy::TR::TR_18(), 0x0050.into())?;
    v.write(phy::TR::TR_17(), 0x100f.into())?;
    v.write(phy::TR::TR_16(), 0x87fa.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0003.into())?;
    v.write(phy::TR::TR_16(), 0x8796.into())?;
    v.write(phy::TR::TR_18(), 0x00c3.into())?;
    v.write(phy::TR::TR_17(), 0xff98.into())?;
    v.write(phy::TR::TR_16(), 0x87f8.into())?;
    v.write(phy::TR::TR_18(), 0x0018.into())?;
    v.write(phy::TR::TR_17(), 0x292a.into())?;
    v.write(phy::TR::TR_16(), 0x8fa4.into())?;
    v.write(phy::TR::TR_18(), 0x00d2.into())?;
    v.write(phy::TR::TR_17(), 0xc46f.into())?;
    v.write(phy::TR::TR_16(), 0x968c.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0620.into())?;
    v.write(phy::TR::TR_16(), 0x97a2.into())?;
    v.write(phy::TR::TR_18(), 0x0013.into())?;
    v.write(phy::TR::TR_17(), 0x132f.into())?;
    v.write(phy::TR::TR_16(), 0x96a4.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0000.into())?;
    v.write(phy::TR::TR_16(), 0x96a8.into())?;
    v.write(phy::TR::TR_18(), 0x00c0.into())?;
    v.write(phy::TR::TR_17(), 0xa028.into())?;
    v.write(phy::TR::TR_16(), 0x8ffc.into())?;
    v.write(phy::TR::TR_18(), 0x0090.into())?;
    v.write(phy::TR::TR_17(), 0x1c09.into())?;
    v.write(phy::TR::TR_16(), 0x8fec.into())?;
    v.write(phy::TR::TR_18(), 0x0004.into())?;
    v.write(phy::TR::TR_17(), 0xa6a1.into())?;
    v.write(phy::TR::TR_16(), 0x8fee.into())?;
    v.write(phy::TR::TR_18(), 0x00b0.into())?;
    v.write(phy::TR::TR_17(), 0x1807.into())?;
    v.write(phy::TR::TR_16(), 0x8ffe.into())?;

    // We're not using 10BASE-TE, so this is the correct config block
    v.write(phy::TR::TR_16(), 0x028e.into())?;
    v.write(phy::TR::TR_18(), 0x0008.into())?;
    v.write(phy::TR::TR_17(), 0xa518.into())?;
    v.write(phy::TR::TR_16(), 0x8486.into())?;
    v.write(phy::TR::TR_18(), 0x006d.into())?;
    v.write(phy::TR::TR_17(), 0xc696.into())?;
    v.write(phy::TR::TR_16(), 0x8488.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0912.into())?;
    v.write(phy::TR::TR_16(), 0x848a.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0db6.into())?;
    v.write(phy::TR::TR_16(), 0x848e.into())?;
    v.write(phy::TR::TR_18(), 0x0059.into())?;
    v.write(phy::TR::TR_17(), 0x6596.into())?;
    v.write(phy::TR::TR_16(), 0x849c.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0514.into())?;
    v.write(phy::TR::TR_16(), 0x849e.into())?;
    v.write(phy::TR::TR_18(), 0x0041.into())?;
    v.write(phy::TR::TR_17(), 0x0280.into())?;
    v.write(phy::TR::TR_16(), 0x84a2.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0000.into())?;
    v.write(phy::TR::TR_16(), 0x84a4.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0000.into())?;
    v.write(phy::TR::TR_16(), 0x84a6.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0000.into())?;
    v.write(phy::TR::TR_16(), 0x84a8.into())?;
    v.write(phy::TR::TR_18(), 0x0000.into())?;
    v.write(phy::TR::TR_17(), 0x0000.into())?;
    v.write(phy::TR::TR_16(), 0x84aa.into())?;
    v.write(phy::TR::TR_18(), 0x007d.into())?;
    v.write(phy::TR::TR_17(), 0xf7dd.into())?;
    v.write(phy::TR::TR_16(), 0x84ae.into())?;
    v.write(phy::TR::TR_18(), 0x006d.into())?;
    v.write(phy::TR::TR_17(), 0x95d4.into())?;
    v.write(phy::TR::TR_16(), 0x84b0.into())?;
    v.write(phy::TR::TR_18(), 0x0049.into())?;
    v.write(phy::TR::TR_17(), 0x2410.into())?;
    v.write(phy::TR::TR_16(), 0x84b2.into())?;

    v.modify(phy::TEST::TEST_PAGE_8(), |r| {
        r.0 &= !0x8000; // Disable token-ring mode
    })?;

    v.modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
        *r = (u16::from(*r) & !1).into();
    })?;

    //////////////////////////////////////////////////////////////////////////
    // Now we're going deep into the weeds.  This section is based on
    // `tesla_revB_8051_patch` in the SDK, which (as the name suggests), patches
    // the 8051 in the PHY.
    const FIRMWARE_START_ADDR: u16 = 0x4000;
    const PATCH_CRC_LEN: u16 = (TESLA_PATCH.len() + 1) as u16;
    const EXPECTED_CRC: u16 = 0x29E8;

    // This patch can only be applied to Port 0 of the PHY, so we'll check
    // the address here.
    let phy_port = v.read(phy::EXTENDED::EXTENDED_PHY_CONTROL_4())?.0 >> 11;
    if phy_port != 0 {
        return Err(VscError::BadPhyPatchPort(phy_port));
    }
    let crc = read_8051_crc(v, FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
    let skip_download = crc == EXPECTED_CRC;
    let patch_ok = skip_download
        && v.read(phy::GPIO::GPIO_3())?.0 == 0x3eb7
        && v.read(phy::GPIO::GPIO_4())?.0 == 0x4012
        && v.read(phy::GPIO::GPIO_12())?.0 == 0x0100
        && v.read(phy::GPIO::GPIO_0())?.0 == 0xc018;
    ringbuf_entry!(Trace::PatchState {
        patch_ok,
        skip_download
    });

    if !skip_download || !patch_ok {
        micro_assert_reset(v)?;
    }
    if !skip_download {
        download_patch(v, &TESLA_PATCH)?;
    }
    if !patch_ok {
        // Various CPU commands to enable the patch
        v.write(phy::GPIO::GPIO_3(), 0x3eb7.into())?;
        v.write(phy::GPIO::GPIO_4(), 0x4012.into())?;
        v.write(phy::GPIO::GPIO_12(), 0x0100.into())?;
        v.write(phy::GPIO::GPIO_0(), 0xc018.into())?;
    }

    if !skip_download {
        let crc = read_8051_crc(v, FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
        if crc != EXPECTED_CRC {
            return Err(VscError::PhyPatchFailedCrc);
        }
    }

    //////////////////////////////////////////////////////////////////////////
    // `vtss_phy_pre_init_tesla_revB_1588`
    //
    // "Pass the cmd to Micro to initialize all 1588 analyzer registers to
    //  default"
    cmd(v, 0x801A)?;

    Ok(())
}

/// Downloads a patch to the 8051 in the PHY, based on `download_8051_code`
/// from the SDK.
fn download_patch<P: PhyRw>(
    v: &mut Phy<P>,
    patch: &[u8],
) -> Result<(), VscError> {
    // "Hold 8051 in SW Reset, Enable auto incr address and patch clock,
    //  Disable the 8051 clock"
    v.write(phy::GPIO::GPIO_0(), 0x7009.into())?;

    // "write to addr 4000 = 02"
    v.write(phy::GPIO::GPIO_12(), 0x5002.into())?;

    // "write to address reg."
    v.write(phy::GPIO::GPIO_11(), 0x0.into())?;

    for &p in patch {
        v.write(phy::GPIO::GPIO_12(), (0x5000 | p as u16).into())?;
    }

    // "Clear internal memory access"
    v.write(phy::GPIO::GPIO_12(), 0.into())?;

    Ok(())
}

/// Based on `vtss_phy_micro_assert_reset`
fn micro_assert_reset<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    // "Pass the NOP cmd to Micro to insure that any consumptive patch exits"
    cmd(v, 0x800F)?;

    // "force micro into a loop, preventing any SMI accesses"
    v.modify(phy::GPIO::GPIO_12(), |r| r.0 &= !0x0800)?;
    v.write(phy::GPIO::GPIO_9(), 0x005b.into())?;
    v.write(phy::GPIO::GPIO_10(), 0x005b.into())?;
    v.modify(phy::GPIO::GPIO_12(), |r| r.0 |= 0x0800)?;
    v.write(phy::GPIO::MICRO_PAGE(), 0x800F.into())?;

    // "Assert reset after micro is trapped in a loop (averts micro-SMI access
    //  deadlock at reset)"
    v.modify(phy::GPIO::GPIO_0(), |r| r.0 &= !0x8000)?;
    v.write(phy::GPIO::MICRO_PAGE(), 0x0000.into())?;
    v.modify(phy::GPIO::GPIO_12(), |r| r.0 &= !0x0800)?;
    Ok(())
}

/// Based on `vtss_phy_is_8051_crc_ok_private`
fn read_8051_crc<P: PhyRw>(
    v: &mut Phy<P>,
    addr: u16,
    size: u16,
) -> Result<u16, VscError> {
    v.write(phy::EXTENDED::VERIPHY_CTRL_REG2(), addr.into())?;
    v.write(phy::EXTENDED::VERIPHY_CTRL_REG3(), size.into())?;

    // Start CRC calculation and wait for it to finish
    cmd(v, 0x8008)?;

    let crc: u16 = v.read(phy::EXTENDED::VERIPHY_CTRL_REG2())?.into();
    ringbuf_entry!(Trace::GotCrc(crc));
    Ok(crc)
}

/// Raw patch for 8051 microcode, from `tesla_revB_8051_patch` in the SDK
const TESLA_PATCH: [u8; 1655] = [
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

/// Raw patch for 8051 microcode, from `viper_revB_8051_patch` in the SDK
const VIPER_PATCH: [u8; 92] = [
    0xe8, 0x59, 0x02, 0xe8, 0x12, 0x02, 0xe8, 0x42, 0x02, 0xe8, 0x5a, 0x02,
    0xe8, 0x5b, 0x02, 0xe8, 0x5c, 0xe5, 0x69, 0x54, 0x0f, 0x24, 0xf7, 0x60,
    0x27, 0x24, 0xfc, 0x60, 0x23, 0x24, 0x08, 0x70, 0x14, 0xe5, 0x69, 0xae,
    0x68, 0x78, 0x04, 0xce, 0xa2, 0xe7, 0x13, 0xce, 0x13, 0xd8, 0xf8, 0x7e,
    0x00, 0x54, 0x0f, 0x80, 0x00, 0x7b, 0x01, 0x7a, 0x00, 0x7d, 0xee, 0x7f,
    0x92, 0x12, 0x50, 0xee, 0x22, 0xe4, 0xf5, 0x10, 0x85, 0x10, 0xfb, 0x7d,
    0x1c, 0xe4, 0xff, 0x12, 0x59, 0xea, 0x05, 0x10, 0xe5, 0x10, 0xc3, 0x94,
    0x04, 0x40, 0xed, 0x22, 0x22, 0x22, 0x22, 0x22,
];
