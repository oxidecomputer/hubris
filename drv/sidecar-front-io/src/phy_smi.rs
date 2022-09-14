// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use vsc85xx::{PhyRw, VscError};
use zerocopy::{byteorder, AsBytes, FromBytes, Unaligned, U16};

pub struct PhySmi {
    fpga: FpgaUserDesign,
    await_not_busy_sleep_for: u64,
}

impl PhySmi {
    pub fn new(fpga_task: userlib::TaskId) -> Self {
        Self {
            // PHY SMI interface is only present/connected on FPGA1.
            fpga: FpgaUserDesign::new(fpga_task, 1),
            await_not_busy_sleep_for: 0,
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
    pub fn set_phy_coma_mode(&self, asserted: bool) -> Result<(), FpgaError> {
        self.fpga.write(
            WriteOp::from(asserted),
            Addr::VSC8562_PHY_CTRL,
            Reg::VSC8562::PHY_CTRL::COMA_MODE,
        )
    }

    #[inline]
    pub fn phy_powered_up_and_ready(&self) -> Result<bool, FpgaError> {
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
            if self.await_not_busy_sleep_for > 0 {
                userlib::hl::sleep_for(self.await_not_busy_sleep_for);
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn read_raw_inner(&self, phy: u8, reg: u8) -> Result<u16, FpgaError> {
        let request = SmiRequest {
            rdata: U16::new(0),
            wdata: U16::new(0),
            phy,
            reg,
            ctrl: Reg::VSC8562::PHY_SMI_CTRL::START,
        };
        self.await_not_busy()?;
        self.fpga.write(
            WriteOp::Write,
            Addr::VSC8562_PHY_SMI_RDATA_H,
            request,
        )?;

        self.await_not_busy()?;
        let v = u16::from_be(self.fpga.read(Addr::VSC8562_PHY_SMI_RDATA_H)?);

        Ok(v)
    }

    #[inline(never)]
    fn write_raw_inner(
        &self,
        phy: u8,
        reg: u8,
        value: u16,
    ) -> Result<(), FpgaError> {
        let request = SmiRequest {
            rdata: U16::new(0),
            wdata: U16::new(value),
            phy,
            reg,
            ctrl: Reg::VSC8562::PHY_SMI_CTRL::RW
                | Reg::VSC8562::PHY_SMI_CTRL::START,
        };
        self.await_not_busy()?;
        self.fpga
            .write(WriteOp::Write, Addr::VSC8562_PHY_SMI_RDATA_H, request)
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

#[derive(AsBytes, FromBytes, Unaligned)]
#[repr(C)]
struct SmiRequest {
    rdata: U16<byteorder::BigEndian>,
    wdata: U16<byteorder::BigEndian>,
    phy: u8,
    reg: u8,
    ctrl: u8,
}
