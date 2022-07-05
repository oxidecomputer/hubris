// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This crate provides functions for working with VSC85xx PHYs
//! (in particular, the VSC8522, VSC8504, and VSC8552)
//!
//! It relies heavily on the trait [PhyRw], which callers must implement.  This
//! trait is an abstraction over reading and writing raw PHY registers.
#![no_std]

mod atom;
mod led;
mod tesla;
mod util;
mod viper;
mod vsc8552;

// User-facing handles to various PHY types
pub mod vsc8504;
pub mod vsc8522;
pub mod vsc8562;
pub mod vsc85x2;

use core::cell::Cell;
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
}

/// Handle for interacting with a particular PHY port.  This handle assumes
/// exclusive access to the port, because it tracks the current page and
/// minimizes page-change writes.  This is _somewhat_ enforced by the ownership
/// rules, as we have an exclusive (mutable) reference to the `PhyRw` object
/// `rw`.
pub struct Phy<'a, P> {
    pub port: u8,
    pub rw: &'a mut P,
    last_page: Cell<Option<u16>>,
}

impl<'a, P: PhyRw> Phy<'a, P> {
    pub fn new(port: u8, rw: &'a mut P) -> Self {
        Self {
            port,
            rw,
            last_page: Cell::new(None),
        }
    }

    /// Sets the PAGE register if it doesn't match.  This assumes that no one
    /// else is allowed to modify the PHY registers, which is mentioned in the
    /// `struct Phy` docstring.
    #[inline(always)]
    fn set_page(&self, page: u16) -> Result<(), VscError> {
        if self.last_page.get().map(|p| p != page).unwrap_or(true) {
            self.rw.write_raw::<phy::standard::PAGE>(
                self.port,
                phy::STANDARD::PAGE(),
                page.into(),
            )?;
            self.last_page.set(Some(page));
        }
        Ok(())
    }

    #[inline(always)]
    pub fn read<T>(&self, reg: PhyRegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        self.set_page(reg.page)?;
        self.rw.read_raw(self.port, reg)
    }

    #[inline(always)]
    pub fn write<T>(
        &self,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        self.set_page(reg.page)?;
        self.rw.write_raw(self.port, reg, value)
    }

    #[inline(always)]
    pub fn write_with<T, F>(
        &self,
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
        &self,
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
        &self,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(T) -> Result<bool, VscError>,
    {
        for _ in 0..32 {
            let r = self.read(reg)?;
            if f(r)? {
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
    Vsc8504Init(u8),
    Vsc8522Init(u8),
    Vsc8552Init(u8),
    Vsc8562InitSgmii(u8),
    Vsc8562InitQsgmii(u8),
    TeslaPatch(u8),
    ViperPatch(u8),
    AtomPatchSuspend(bool),
    AtomPatchResume(bool),
    PatchState { patch_ok: bool, skip_download: bool },
    GotCrc(u16),
}
ringbuf!(Trace, 16, Trace::None);

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
