// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This crate provides functions for working with VSC85xx PHYs
//! (in particular, the VSC8522, VSC8504, and VSC8552)
//!
//! It relies heavily on the trait [PhyRw], which callers must implement.  This
//! trait is an abstraction over reading and writing raw PHY registers.
#![no_std]

mod led;
mod tesla;
mod util;
mod viper;
mod vsc8552;
mod vsc8562;
pub mod vsc85x2;

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

/// Handle for interacting with a particular PHY port.  This handle assumes
/// exclusive access to the port, because it tracks the current page and
/// minimizes page-change writes.  This is _somewhat_ enforced by the ownership
/// rules, as we have an exclusive (mutable) reference to the `PhyRw` object
/// `rw`.
pub struct Phy<'a, P> {
    pub port: u8,
    pub rw: &'a mut P,
    last_page: Option<u16>,
}

impl<'a, P: PhyRw> Phy<'a, P> {
    pub fn new(port: u8, rw: &'a mut P) -> Self {
        Self {
            port,
            rw,
            last_page: None,
        }
    }

    #[inline(always)]
    pub fn read<T>(&mut self, reg: PhyRegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        if self.last_page.map(|p| p != reg.page).unwrap_or(true) {
            self.rw.write_raw::<phy::standard::PAGE>(
                self.port,
                phy::STANDARD::PAGE(),
                reg.page.into(),
            )?;
            self.last_page = Some(reg.page);
        }
        self.rw.read_raw(self.port, reg)
    }

    #[inline(always)]
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

    #[inline(always)]
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
    #[inline(always)]
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

    #[inline(always)]
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
    TeslaPatch(u8),
    ViperPatch(u8),
    Vsc8552Init(u8),
    Vsc8562Init(u8),
    Vsc8504Init(u8),
    PatchState { patch_ok: bool, skip_download: bool },
    GotCrc(u16),
}
ringbuf!(Trace, 16, Trace::None);

////////////////////////////////////////////////////////////////////////////////

// These IDs are (id1 << 16) | id2, meaning they also capture device revision
// number.  This matters, because the patches are device-revision specific.
const VSC8504_ID: u32 = 0x704c2;
const VSC8522_ID: u32 = 0x706f3;

/// Initializes a VSC8522 PHY using QSGMII.
/// This is the PHY on the VSC7448 dev kit.
pub fn init_vsc8522_phy<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8522Init(v.port));

    let id = v.read_id()?;
    if id != VSC8522_ID {
        return Err(VscError::BadPhyId(id));
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
///
/// This must be called on the base port of the PHY, and will configure all
/// ports using broadcast writes.
pub fn init_vsc8504_phy<P: PhyRw>(v: &mut Phy<P>) -> Result<(), VscError> {
    ringbuf_entry!(Trace::Vsc8504Init(v.port));

    let id = v.read_id()?;
    if id != VSC8504_ID {
        return Err(VscError::BadPhyId(id));
    }

    let rev = v.read(phy::GPIO::EXTENDED_REVISION())?;
    if rev.tesla_e() != 1 {
        return Err(VscError::BadPhyRev);
    }

    v.check_base_port()?;
    crate::tesla::TeslaPhy(v).patch()?;

    // Configure MAC in QSGMII mode
    v.broadcast(|v| {
        v.modify(phy::GPIO::MAC_MODE_AND_FAST_LINK(), |r| {
            r.0 = (r.0 & !(0b11 << 14)) | (0b01 << 14)
        })
    })?;

    // Enable 4 port MAC QSGMII
    v.cmd(0x80E0)?;

    // The PHY is already configured for copper in register 23
    // XXX: I don't think this is correct

    // Now, we reset the PHY to put those settings into effect
    // XXX: is it necessary to reset each of the four ports independently?
    // (It _is_ necessary for the VSC8552 on the management network dev board)
    for p in 0..4 {
        Phy::new(v.port + p, v.rw).software_reset()?;
    }

    Ok(())
}

////////////////////////////////////////////////////////////////////////////////

/// Represents the status of an internal PHY counter.  `Unavailable` indicates
/// a counter which isn't available on this particular PHY (in particular,
/// the VSC8552 doesn't have MAC counters); `Inactive` means that the counter
/// is available but the active bit is cleared.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Counter {
    Unavailable,
    Inactive,
    Value(u16),
}

impl Default for Counter {
    fn default() -> Self {
        Self::Inactive
    }
}
