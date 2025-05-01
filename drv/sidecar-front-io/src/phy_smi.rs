// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::cell::Cell;

use crate::{Addr, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use vsc85xx::{PhyRw, VscError};
use zerocopy::{byteorder, FromBytes, IntoBytes, Unaligned, U16};

#[derive(Copy, Clone, Eq, Debug, PartialEq)]
pub enum PhyOscState {
    Unknown,
    Bad,
    Good,
}

pub struct PhySmi {
    fpga: FpgaUserDesign,

    /// Records whether an SMI operation might be in progress, and we should
    /// poll the status register before starting a new operation.
    ///
    /// This is a `Cell` because `PhyRw` functions expect `&self`
    maybe_busy: Cell<bool>,
}

impl PhySmi {
    pub fn new(fpga_task: userlib::TaskId) -> Self {
        Self {
            // PHY SMI interface is only present/connected on FPGA1.
            fpga: FpgaUserDesign::new(fpga_task, 1),

            maybe_busy: Cell::new(true),
        }
    }

    #[inline]
    pub fn phy_status(&self) -> Result<u8, FpgaError> {
        self.fpga.read(Addr::VSC8562_PHY_STATUS)
    }

    #[inline]
    pub fn phy_ctrl(&self) -> Result<u8, FpgaError> {
        self.fpga.read(Addr::VSC8562_PHY_CTRL)
    }

    #[inline]
    pub fn set_phy_ctrl(&self, val: u8) -> Result<(), FpgaError> {
        self.fpga.write(WriteOp::Write, Addr::VSC8562_PHY_CTRL, val)
    }

    #[inline]
    pub fn phy_power_enabled(&self) -> Result<bool, FpgaError> {
        Ok((self.phy_ctrl()? & Reg::VSC8562::PHY_CTRL::EN) != 0)
    }

    #[inline]
    pub fn set_phy_power_enabled(
        &self,
        enabled: bool,
    ) -> Result<(), FpgaError> {
        self.fpga.write(
            WriteOp::from(enabled),
            Addr::VSC8562_PHY_CTRL,
            Reg::VSC8562::PHY_CTRL::EN,
        )
    }

    #[inline]
    pub fn set_coma_mode(&self, asserted: bool) -> Result<(), FpgaError> {
        self.fpga.write(
            WriteOp::from(asserted),
            Addr::VSC8562_PHY_CTRL,
            Reg::VSC8562::PHY_CTRL::COMA_MODE,
        )
    }

    #[inline]
    pub fn powered_up_and_ready(&self) -> Result<bool, FpgaError> {
        let status: u8 = self.fpga.read(Addr::VSC8562_PHY_STATUS)?;
        Ok((status & Reg::VSC8562::PHY_STATUS::READY) != 0)
    }

    #[inline]
    pub fn smi_busy(&self) -> Result<bool, FpgaError> {
        let status: u8 = self.fpga.read(Addr::VSC8562_PHY_SMI_STATUS)?;
        Ok((status & Reg::VSC8562::PHY_SMI_STATUS::BUSY) != 0)
    }

    pub fn await_not_busy(&self) -> Result<(), FpgaError> {
        while self.smi_busy()? {
            // busy-loop, because MDIO is fast
        }
        Ok(())
    }

    pub fn osc_state(&self) -> Result<PhyOscState, FpgaError> {
        let phy_osc: u8 = self.fpga.read(Addr::VSC8562_PHY_OSC)?;

        let good = phy_osc & Reg::VSC8562::PHY_OSC::GOOD != 0;
        let valid = phy_osc & Reg::VSC8562::PHY_OSC::VALID != 0;

        Ok(match (valid, good) {
            (false, _) => PhyOscState::Unknown,
            (true, false) => PhyOscState::Bad,
            (true, true) => PhyOscState::Good,
        })
    }

    pub fn set_osc_good(&self, good: bool) -> Result<(), FpgaError> {
        self.fpga.write(
            WriteOp::Write,
            Addr::VSC8562_PHY_OSC,
            Reg::VSC8562::PHY_OSC::VALID
                | if good { Reg::VSC8562::PHY_OSC::GOOD } else { 0 },
        )
    }

    #[inline(never)]
    fn read_raw_inner(&self, phy: u8, reg: u8) -> Result<u16, FpgaError> {
        let request = SmiReadRequest {
            phy,
            reg,
            ctrl: Reg::VSC8562::PHY_SMI_CTRL::START,
        };

        if self.maybe_busy.replace(true) {
            self.await_not_busy()?;
        }

        self.fpga.write(
            WriteOp::Write,
            Addr::VSC8562_PHY_SMI_PHY_ADDR,
            request,
        )?;

        // We _could_ use `await_not_busy` here, but that means doing two
        // transactions: one to check the status, and a second to read back the
        // data.  The status and rdata register are contiguous in memory, and
        // we're _almost always_ ready in the first poll, so let's instead read
        // all three bytes together.
        loop {
            let r: SmiReadData =
                self.fpga.read(Addr::VSC8562_PHY_SMI_STATUS)?;
            if (r.status & Reg::VSC8562::PHY_SMI_STATUS::BUSY) == 0 {
                self.maybe_busy.set(false);
                return Ok(r.rdata.get());
            }
        }
    }

    #[inline(never)]
    fn write_raw_inner(
        &self,
        phy: u8,
        reg: u8,
        value: u16,
    ) -> Result<(), FpgaError> {
        let request = SmiWriteRequest {
            wdata: U16::new(value),
            phy,
            reg,
            ctrl: Reg::VSC8562::PHY_SMI_CTRL::RW
                | Reg::VSC8562::PHY_SMI_CTRL::START,
        };
        if self.maybe_busy.replace(true) {
            self.await_not_busy()?;
        }

        self.fpga
            .write(WriteOp::Write, Addr::VSC8562_PHY_SMI_WDATA0, request)
    }
}

impl PhyRw for PhySmi {
    #[inline(always)]
    fn read_raw(&self, phy: u8, reg: u8) -> Result<u16, VscError> {
        self.read_raw_inner(phy, reg)
            .map_err(|e| VscError::ProxyError(e.into()))
    }

    #[inline(always)]
    fn write_raw(&self, phy: u8, reg: u8, value: u16) -> Result<(), VscError> {
        self.write_raw_inner(phy, reg, value)
            .map_err(|e| VscError::ProxyError(e.into()))
    }
}

#[derive(IntoBytes, FromBytes, Unaligned)]
#[repr(C)]
struct SmiWriteRequest {
    wdata: U16<byteorder::LittleEndian>,
    phy: u8,
    reg: u8,
    ctrl: u8,
}

#[derive(IntoBytes, FromBytes, Unaligned)]
#[repr(C)]
struct SmiReadRequest {
    phy: u8,
    reg: u8,
    ctrl: u8,
}

#[derive(IntoBytes, FromBytes, Unaligned, Default)]
#[repr(C)]
struct SmiReadData {
    status: u8,
    rdata: U16<byteorder::LittleEndian>,
}
