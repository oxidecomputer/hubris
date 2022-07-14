// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Phy, PhyRw, Trace};

use ringbuf::ringbuf_entry_root as ringbuf_entry;
use vsc7448_pac::{phy, types::PhyRegisterAddress};
use vsc_err::VscError;

impl<'a, P: PhyRw> Phy<'a, P> {
    pub fn read_id(&self) -> Result<u32, VscError> {
        let id1 = self.read(phy::STANDARD::IDENTIFIER_1())?.0;
        let id2 = self.read(phy::STANDARD::IDENTIFIER_2())?.0;
        Ok((u32::from(id1) << 16) | u32::from(id2))
    }

    pub(crate) fn software_reset(&self) -> Result<(), VscError> {
        self.modify(phy::STANDARD::MODE_CONTROL(), |r| {
            r.set_sw_reset(1);
        })?;
        self.wait_timeout(phy::STANDARD::MODE_CONTROL(), |r| {
            Ok(r.sw_reset() != 1)
        })
    }

    /// The VSC85xx family supports sending commands to the system by writing to
    /// register 19G.  This helper function sends a command then waits for it
    /// to finish, return [VscError::PhyInitTimeout] if it fails (or another
    /// [VscError] if communication to the PHY doesn't work)
    pub(crate) fn cmd(&self, command: u16) -> Result<(), VscError> {
        self.write(phy::GPIO::MICRO_PAGE(), command.into())?;
        self.wait_timeout(phy::GPIO::MICRO_PAGE(), |r| {
            if r.0 & 0x4000 != 0 {
                Err(VscError::PhyCommandError(command))
            } else {
                Ok(r.0 & 0x8000 == 0)
            }
        })?;
        Ok(())
    }

    /// Checks whether `v` is the base port of the PHY, returning an error if
    /// that's not the case.
    pub(crate) fn check_base_port(&self) -> Result<(), VscError> {
        let phy_port = self.get_port()?;
        if phy_port == 0 {
            Ok(())
        } else {
            Err(VscError::BadPhyPatchPort(phy_port))
        }
    }

    /// Returns the (internal) PHY's port number, starting from 0
    pub(crate) fn get_port(&self) -> Result<u16, VscError> {
        Ok(self.read(phy::EXTENDED::EXTENDED_PHY_CONTROL_4())?.0 >> 11)
    }

    /// Calls a function with broadcast writes enabled, then unsets the flag
    pub(crate) fn broadcast<F: Fn(&Phy<P>) -> Result<(), VscError>>(
        &self,
        f: F,
    ) -> Result<(), VscError> {
        // Set the broadcast flag
        self.modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
            *r = (u16::from(*r) | 1).into()
        })?;
        let result = f(self);

        // Undo the broadcast flag even if the function failed for some reason
        self.modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
            *r = (u16::from(*r) & !1).into()
        })?;
        result
    }

    /// Downloads a patch to the 8051 in the PHY, based on `download_8051_code`
    /// from the SDK.
    pub(crate) fn download_patch(&self, patch: &[u8]) -> Result<(), VscError> {
        // "Hold 8051 in SW Reset, Enable auto incr address and patch clock,
        //  Disable the 8051 clock"
        self.write(phy::GPIO::GPIO_0(), 0x7009.into())?;

        // "write to addr 4000 = 02"
        self.write(phy::GPIO::GPIO_12(), 0x5002.into())?;

        // "write to address reg."
        self.write(phy::GPIO::GPIO_11(), 0x0.into())?;

        for &p in patch {
            self.write(phy::GPIO::GPIO_12(), (0x5000 | p as u16).into())?;
        }

        // "Clear internal memory access"
        self.write(phy::GPIO::GPIO_12(), 0.into())?;

        Ok(())
    }

    /// Based on `vtss_phy_micro_assert_reset`
    pub(crate) fn micro_assert_reset(&self) -> Result<(), VscError> {
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
    pub(crate) fn read_8051_crc(
        &self,
        addr: u16,
        size: u16,
    ) -> Result<u16, VscError> {
        self.write(phy::EXTENDED::VERIPHY_CTRL_REG2(), addr.into())?;
        self.write(phy::EXTENDED::VERIPHY_CTRL_REG3(), size.into())?;

        // Start CRC calculation and wait for it to finish
        self.cmd(0x8008)?;

        let crc: u16 = self.read(phy::EXTENDED::VERIPHY_CTRL_REG2())?.into();
        ringbuf_entry!(Trace::GotCrc(crc));
        Ok(crc)
    }
}

/// Helper const fn to strip the type from a PhyRegisterAddress, used when
/// packing them into an array.
pub(crate) const fn detype<T>(r: PhyRegisterAddress<T>) -> (u16, u8) {
    (r.page, r.addr)
}
