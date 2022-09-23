// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, MainboardController, Reg};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use userlib::FromPrimitive;
use zerocopy::AsBytes;

#[derive(Copy, Clone, Eq, PartialEq, FromPrimitive, AsBytes)]
#[repr(u8)]
pub enum TofinoSeqState {
    Initial = 0,
    A2 = 1,
    A0 = 2,
    InPowerUp = 3,
    InPowerDown = 4,
}

#[derive(Copy, Clone, Eq, PartialEq, FromPrimitive, AsBytes)]
#[repr(u8)]
pub enum TofinoSeqError {
    None = 0,
    PowerGoodTimeout = 1,
    PowerFault = 2,
    PowerVrHot = 3,
    PowerInvalidState = 4,
    UserAbort = 5,
    VidAckTimeout = 6,
    ThermalAlert = 7,
}

/// VID to voltage mapping. The VID values are specified in TF2-DS2, with the
/// actual voltage values derived experimentally after load testing the PDN.
#[derive(Copy, Clone, Eq, PartialEq, FromPrimitive, AsBytes)]
#[repr(u8)]
pub enum Tofino2Vid {
    V0P922 = 0b1111,
    V0P893 = 0b1110,
    V0P867 = 0b1101,
    V0P847 = 0b1100,
    V0P831 = 0b1011,
    V0P815 = 0b1010,
    V0P790 = 0b1001,
    V0P759 = 0b1000,
}

pub struct Sequencer {
    fpga: FpgaUserDesign,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Status {
    state: TofinoSeqState,
    error: TofinoSeqError,
    vid: Option<Tofino2Vid>,
    power: u32,
    pcie_status: u8,
}

#[derive(Copy, Clone, Eq, PartialEq, FromPrimitive, AsBytes)]
#[repr(u8)]
pub enum TofinoPcieReset {
    HostControl,
    Asserted,
    Deasserted,
}

impl Sequencer {
    pub fn new(task_id: userlib::TaskId) -> Self {
        Self {
            fpga: FpgaUserDesign::new(
                task_id,
                MainboardController::DEVICE_INDEX,
            ),
        }
    }

    fn read_masked(&self, addr: Addr, mask: u8) -> Result<u8, FpgaError> {
        let v: u8 = self.fpga.read(addr)?;
        Ok(v & mask)
    }

    fn write_ctrl(&self, op: WriteOp, value: u8) -> Result<(), FpgaError> {
        self.fpga.write(op, Addr::TOFINO_SEQ_CTRL, value)
    }

    pub fn clear_error(&self) -> Result<(), FpgaError> {
        self.write_ctrl(WriteOp::BitSet, Reg::TOFINO_SEQ_CTRL::CLEAR_ERROR)
    }

    pub fn enabled(&self) -> Result<bool, FpgaError> {
        self.read_masked(Addr::TOFINO_SEQ_CTRL, Reg::TOFINO_SEQ_CTRL::EN)
            .map(|v| v != 0)
    }

    pub fn set_enable(&self, enabled: bool) -> Result<(), FpgaError> {
        let op = if enabled {
            WriteOp::BitSet
        } else {
            WriteOp::BitClear
        };
        self.write_ctrl(op, Reg::TOFINO_SEQ_CTRL::EN)
    }

    pub fn ack_vid(&self) -> Result<(), FpgaError> {
        self.write_ctrl(WriteOp::BitSet, Reg::TOFINO_SEQ_CTRL::ACK_VID)
    }

    pub fn state(&self) -> Result<TofinoSeqState, FpgaError> {
        let v = self.read_masked(
            Addr::TOFINO_SEQ_STATE,
            Reg::TOFINO_SEQ_STATE::STATE,
        )?;
        TofinoSeqState::from_u8(v).ok_or(FpgaError::InvalidValue)
    }

    pub fn error(&self) -> Result<TofinoSeqError, FpgaError> {
        let v = self.read_masked(
            Addr::TOFINO_SEQ_ERROR,
            Reg::TOFINO_SEQ_ERROR::ERROR,
        )?;
        TofinoSeqError::from_u8(v).ok_or(FpgaError::InvalidValue)
    }

    pub fn power_status(&self) -> Result<u32, FpgaError> {
        self.fpga.read(Addr::TOFINO_POWER_ENABLE)
    }

    /// The VID is only valid once Tofino is powered up and a delay after PoR
    /// has lapsed. If the VID is read while in this state a `Some(..)` will be
    /// returned. Attempting to read the VID outside this window will result in
    /// `None`. An `FpgaError` is returned if communication with the mainboard
    /// controller failed or an invalid value was read from the register.
    pub fn vid(&self) -> Result<Option<Tofino2Vid>, FpgaError> {
        let v: u8 = self.fpga.read(Addr::TOFINO_POWER_VID)?;

        if (v & Reg::TOFINO_POWER_VID::VID_VALID) != 0 {
            match Tofino2Vid::from_u8(v & Reg::TOFINO_POWER_VID::VID) {
                None => Err(FpgaError::InvalidValue),
                some_vid => Ok(some_vid),
            }
        } else {
            Ok(None)
        }
    }

    pub fn pcie_hotplug_ctrl(&self) -> Result<u8, FpgaError> {
        self.fpga.read(Addr::PCIE_HOTPLUG_CTRL)
    }

    pub fn write_pcie_hotplug_ctrl(
        &self,
        op: WriteOp,
        value: u8,
    ) -> Result<(), FpgaError> {
        self.fpga.write(op, Addr::PCIE_HOTPLUG_CTRL, value)
    }

    pub fn set_pcie_present(&self, present: bool) -> Result<(), FpgaError> {
        self.write_pcie_hotplug_ctrl(
            present.into(),
            Reg::PCIE_HOTPLUG_CTRL::PRESENT,
        )
    }

    pub fn set_pcie_power_fault(&self, fault: bool) -> Result<(), FpgaError> {
        self.write_pcie_hotplug_ctrl(
            fault.into(),
            Reg::PCIE_HOTPLUG_CTRL::POWER_FAULT,
        )
    }

    pub fn set_pcie_alert(&self, alert: bool) -> Result<(), FpgaError> {
        self.write_pcie_hotplug_ctrl(
            alert.into(),
            Reg::PCIE_HOTPLUG_CTRL::ALERT,
        )
    }

    pub fn pcie_reset(&self) -> Result<TofinoPcieReset, FpgaError> {
        let ctrl = self.pcie_hotplug_ctrl()?;
        let reset = (ctrl & Reg::PCIE_HOTPLUG_CTRL::RESET) != 0;
        let override_host_reset =
            (ctrl & Reg::PCIE_HOTPLUG_CTRL::OVERRIDE_HOST_RESET) != 0;

        match (override_host_reset, reset) {
            (false, _) => Ok(TofinoPcieReset::HostControl),
            (true, false) => Ok(TofinoPcieReset::Deasserted),
            (true, true) => Ok(TofinoPcieReset::Asserted),
        }
    }

    pub fn set_pcie_reset(
        &self,
        reset: TofinoPcieReset,
    ) -> Result<(), FpgaError> {
        let ctrl = self.pcie_hotplug_ctrl()?;
        let ctrl_next = match reset {
            TofinoPcieReset::HostControl => {
                // Clear RESET, OVERRIDE_HOST_RESET.
                ctrl & !(Reg::PCIE_HOTPLUG_CTRL::RESET
                    | Reg::PCIE_HOTPLUG_CTRL::OVERRIDE_HOST_RESET)
            }
            TofinoPcieReset::Asserted => {
                // Set RESET, OVERRIDE_HOST_RESET.
                ctrl | Reg::PCIE_HOTPLUG_CTRL::RESET
                    | Reg::PCIE_HOTPLUG_CTRL::OVERRIDE_HOST_RESET
            }
            TofinoPcieReset::Deasserted => {
                // Set HOST_OVERRIDE_RESET, clear RESET.
                (ctrl & !Reg::PCIE_HOTPLUG_CTRL::RESET)
                    | Reg::PCIE_HOTPLUG_CTRL::OVERRIDE_HOST_RESET
            }
        };

        self.write_pcie_hotplug_ctrl(WriteOp::Write, ctrl_next)
    }

    pub fn pcie_hotplug_status(&self) -> Result<u8, FpgaError> {
        self.fpga.read(Addr::PCIE_HOTPLUG_STATUS)
    }

    pub fn status(&self) -> Result<Status, FpgaError> {
        Ok(Status {
            state: self.state()?,
            error: self.error()?,
            vid: self.vid()?,
            power: self.power_status()?,
            pcie_status: self.pcie_hotplug_status()?,
        })
    }
}
