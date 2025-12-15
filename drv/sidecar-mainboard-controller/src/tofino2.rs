// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, MainboardController, Reg};
use bitfield::bitfield;
use derive_more::{From, Into};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use drv_fpga_user_api::power_rail::*;
use userlib::FromPrimitive;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

#[derive(Copy, Clone, PartialEq)]
enum VidTrace {
    None,
    Read(u8),
}

ringbuf::ringbuf!(VidTrace, 8, VidTrace::None);

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    FromPrimitive,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u8)]
pub enum TofinoSeqState {
    #[default]
    Init = 0,
    A2 = 1,
    A0 = 2,
    InPowerUp = 3,
    InPowerDown = 4,
}

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    FromPrimitive,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u8)]
pub enum TofinoSeqStep {
    #[default]
    Init = 0,
    AwaitPowerUp = 1,
    AwaitVdd18PowerGood = 2,
    AwaitVddCorePowerGood = 3,
    AwaitVddPciePowerGood = 4,
    AwaitVddtPowerGood = 5,
    AwaitVdda15PowerGood = 6,
    AwaitVdda18PowerGood = 7,
    AwaitPoR = 8,
    AwaitVidValid = 9,
    AwaitVidAck = 10,
    AwaitPowerUpComplete = 11,
    AwaitPowerDown = 12,
    AwaitPowerDownComplete = 13,
}

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    FromPrimitive,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u8)]
pub enum TofinoSeqError {
    #[default]
    None = 0,
    PowerGoodTimeout = 1,
    PowerFault = 2,
    PowerVrHot = 3,
    PowerAbort = 4,
    SoftwareAbort = 5,
    VidAckTimeout = 6,
    ThermalAlert = 7,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct TofinoSeqStatus {
    pub state: TofinoSeqState,
    pub step: TofinoSeqStep,
    pub abort: Option<TofinoSeqAbort>,
}

/// Decode the Tofino status from raw register data.
impl TryFrom<[u8; 6]> for TofinoSeqStatus {
    type Error = FpgaError;

    fn try_from(data: [u8; 6]) -> Result<TofinoSeqStatus, Self::Error> {
        let value = |addr: Addr, mask: u8| {
            data[(addr as usize) - (Addr::TOFINO_SEQ_CTRL as usize)] & mask
        };

        let state = TofinoSeqState::from_u8(value(
            Addr::TOFINO_SEQ_STATE,
            Reg::TOFINO_SEQ_STATE::STATE,
        ))
        .ok_or(FpgaError::InvalidValue)?;

        let step = TofinoSeqStep::from_u8(value(
            Addr::TOFINO_SEQ_STEP,
            Reg::TOFINO_SEQ_STEP::STEP,
        ))
        .ok_or(FpgaError::InvalidValue)?;

        let error = TofinoSeqError::from_u8(value(
            Addr::TOFINO_SEQ_ERROR,
            Reg::TOFINO_SEQ_ERROR::ERROR,
        ))
        .ok_or(FpgaError::InvalidValue)?;

        let error_state = TofinoSeqState::from_u8(value(
            Addr::TOFINO_SEQ_ERROR_STATE,
            Reg::TOFINO_SEQ_ERROR_STATE::STATE,
        ))
        .ok_or(FpgaError::InvalidValue)?;

        let error_step = TofinoSeqStep::from_u8(value(
            Addr::TOFINO_SEQ_ERROR_STEP,
            Reg::TOFINO_SEQ_ERROR_STEP::STEP,
        ))
        .ok_or(FpgaError::InvalidValue)?;

        Ok(TofinoSeqStatus {
            state,
            step,
            abort: match error {
                TofinoSeqError::None => None,
                _ => Some(TofinoSeqAbort {
                    state: error_state,
                    step: error_step,
                    error,
                }),
            },
        })
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct TofinoSeqAbort {
    pub state: TofinoSeqState,
    pub step: TofinoSeqStep,
    pub error: TofinoSeqError,
}

#[derive(Copy, Clone, Debug, Eq, FromPrimitive, PartialEq)]
#[repr(C)]
// These id's correspond to the order of status registers in the mainboard
// controller register map and are used to attach the power rail "name" to fault
// data sent upstack.
pub enum TofinoPowerRailId {
    Vdd18 = 0,
    VddCore = 1,
    VddPcie = 2,
    Vddt = 3,
    Vdda15 = 4,
    Vdda18 = 5,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TofinoPowerRail {
    pub id: TofinoPowerRailId,
    pub status: PowerRailStatus,
    pub pins: PowerRailPinState,
}

/// VID to voltage mapping. The VID values are specified in TF2-DS2, with the
/// actual voltage values derived experimentally after load testing the PDN.
#[derive(
    Copy, Clone, Eq, PartialEq, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
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
    step: TofinoSeqStep,
    abort: Option<TofinoSeqAbort>,
}

#[derive(
    Copy, Clone, Eq, PartialEq, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
#[repr(u8)]
pub enum TofinoPcieReset {
    HostControl,
    Asserted,
    Deasserted,
}

#[derive(
    Copy, Clone, Eq, PartialEq, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
#[repr(u8)]
pub enum TofinoPciePowerFault {
    SequencerControl,
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

    pub fn error_step(&self) -> Result<TofinoSeqStep, FpgaError> {
        let v = self.read_masked(
            Addr::TOFINO_SEQ_ERROR_STEP,
            Reg::TOFINO_SEQ_ERROR_STEP::STEP,
        )?;
        TofinoSeqStep::from_u8(v).ok_or(FpgaError::InvalidValue)
    }

    #[inline]
    pub fn raw_status(&self) -> Result<[u8; 6], FpgaError> {
        self.fpga.read(Addr::TOFINO_SEQ_CTRL)
    }

    #[inline]
    pub fn status(&self) -> Result<TofinoSeqStatus, FpgaError> {
        let data = self.raw_status()?;
        TofinoSeqStatus::try_from(data)
    }

    #[inline]
    pub fn power_rail_states(
        &self,
    ) -> Result<[RawPowerRailState; 6], FpgaError> {
        self.fpga.read(Addr::TOFINO_POWER_VDD18_STATE)
    }

    pub fn power_rails(&self) -> Result<[TofinoPowerRail; 6], FpgaError> {
        let power_rail_states = self.power_rail_states()?;
        let mut maybe_power_rails = [None; 6];

        for (i, o) in maybe_power_rails.iter_mut().enumerate() {
            let id = TofinoPowerRailId::from_usize(i)
                .ok_or(FpgaError::InvalidValue)?;
            let status = PowerRailStatus::try_from(power_rail_states[i])?;
            let pins = PowerRailPinState::from(power_rail_states[i]);

            *o = Some(TofinoPowerRail { id, status, pins });
        }

        Ok(maybe_power_rails.map(Option::unwrap))
    }

    /// The VID is only valid once Tofino is powered up and a delay after PoR
    /// has lapsed. If the VID is read while in this state a `Some(..)` will be
    /// returned. Attempting to read the VID outside this window will result in
    /// `None`. An `FpgaError` is returned if communication with the mainboard
    /// controller failed or an invalid value was read from the register.
    pub fn vid(&self) -> Result<Option<Tofino2Vid>, FpgaError> {
        let v: u8 = self.fpga.read(Addr::TOFINO_POWER_VID)?;

        ringbuf::ringbuf_entry!(VidTrace::Read(v));

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
                // Set OVERRIDE_HOST_RESET, clear RESET.
                (ctrl & !Reg::PCIE_HOTPLUG_CTRL::RESET)
                    | Reg::PCIE_HOTPLUG_CTRL::OVERRIDE_HOST_RESET
            }
        };

        self.write_pcie_hotplug_ctrl(WriteOp::Write, ctrl_next)
    }

    pub fn set_pcie_power_fault(
        &self,
        power_fault: TofinoPciePowerFault,
    ) -> Result<(), FpgaError> {
        let ctrl = self.pcie_hotplug_ctrl()?;
        let ctrl_next = match power_fault {
            TofinoPciePowerFault::SequencerControl => {
                // Clear POWER_FAULT, OVERRIDE_SEQ_POWER_FAULT.
                ctrl & !(Reg::PCIE_HOTPLUG_CTRL::POWER_FAULT
                    | Reg::PCIE_HOTPLUG_CTRL::OVERRIDE_SEQ_POWER_FAULT)
            }
            TofinoPciePowerFault::Asserted => {
                // Set POWER_FAULT, OVERRIDE_SEQ_POWER_FAULT.
                ctrl | Reg::PCIE_HOTPLUG_CTRL::POWER_FAULT
                    | Reg::PCIE_HOTPLUG_CTRL::OVERRIDE_SEQ_POWER_FAULT
            }
            TofinoPciePowerFault::Deasserted => {
                // Set OVERRIDE_SEQ_POWER_FAULT, clear POWER_FAULT.
                (ctrl & !Reg::PCIE_HOTPLUG_CTRL::POWER_FAULT)
                    | Reg::PCIE_HOTPLUG_CTRL::OVERRIDE_SEQ_POWER_FAULT
            }
        };

        self.write_pcie_hotplug_ctrl(WriteOp::Write, ctrl_next)
    }

    pub fn pcie_hotplug_status(&self) -> Result<u8, FpgaError> {
        self.fpga.read(Addr::PCIE_HOTPLUG_STATUS)
    }
}

bitfield! {
    #[derive(
        Copy,
        Clone,
        PartialEq,
        Eq,
        FromPrimitive,
        IntoBytes,
        FromBytes,
        Immutable,
        KnownLayout,
    )]
    #[repr(C)]
    pub struct DebugPortState(u8);
    pub send_buffer_empty, set_send_buffer_empty: 0;
    pub send_buffer_full, _: 1;
    pub receive_buffer_empty, set_receive_buffer_empty: 2;
    pub receive_buffer_full, _: 3;
    pub request_in_progress, set_request_in_progress: 4;
    pub address_nack_error, set_address_nack_error: 5;
    pub byte_nack_error, set_byte_nack_error: 6;
}

#[derive(
    Copy, Clone, PartialEq, Eq, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
#[repr(u8)]
pub enum DebugRequestOpcode {
    LocalWrite = 0b0000_0000,
    LocalRead = 0b0010_0000,
    DirectWrite = 0b1000_0000,
    DirectRead = 0b1010_0000,
    IndirectWrite = 0b1100_0000,
    IndirectRead = 0b1110_0000,
}

impl From<DebugRequestOpcode> for u8 {
    fn from(opcode: DebugRequestOpcode) -> Self {
        opcode as u8
    }
}

#[derive(
    Copy, Clone, PartialEq, Eq, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
#[repr(u32)]
pub enum DirectBarSegment {
    Bar0 = 0,
    Msi = 1 << 28,
    Cfg = 2 << 28,
}

/// A few of the Tofino registers which are used in code below. These are found
/// in 631384-0001_TF2-Top-Level_Register_Map_05062021.html as provided by
/// Intel.
#[derive(
    Copy, Clone, PartialEq, Eq, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
#[repr(u32)]
pub enum TofinoBar0Registers {
    Scratchpad = 0x0,
    FreeRunningCounter = 0x10,
    PcieDevInfo = 0x180,
    SoftwareReset = (0x80000 | 0x0),
    ResetOptions = (0x80000 | 0x4),
    PciePhyLaneControl0 = (0x80000 | 0x38),
    PciePhyLaneControl1 = (0x80000 | 0x3c),
    PciePhyLaneStatus0 = (0x80000 | 0x40),
    PciePhyLaneStatus1 = (0x80000 | 0x44),
    SpiOutData0 = (0x80000 | 0x120),
    SpiOutData1 = (0x80000 | 0x124),
    SpiInData = (0x80000 | 0x12c),
    SpiCommand = (0x80000 | 0x128),
    SpiIdCode = (0x80000 | 0x130),
}

impl From<TofinoBar0Registers> for u32 {
    fn from(r: TofinoBar0Registers) -> Self {
        r as u32
    }
}

#[derive(
    Copy, Clone, PartialEq, Eq, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
#[repr(u32)]
pub enum TofinoCfgRegisters {
    KGen = 0x0,
}

impl From<TofinoCfgRegisters> for u32 {
    fn from(r: TofinoCfgRegisters) -> Self {
        r as u32
    }
}

bitfield! {
    /// The `SoftwareReset` register allows for software control over the reset
    /// sequence of Tofino. Note that not all fields are represented in this
    /// struct. See the full set in
    /// 631384-0001_TF2-Top-Level_Register_Map_05062021.html if additional ones
    /// are desired.
    #[derive(
        Copy,
        Clone,
        PartialEq,
        Eq,
        From,
        Into,
        FromPrimitive,
        IntoBytes,
        FromBytes,
        Immutable,
        KnownLayout,
    )]
    #[repr(C)]
    pub struct SoftwareReset(u32);
    pub pcie_phy, set_pcie_phy: 0;
    pub pcie_ctrl, set_pcie_ctrl: 2;
    pub pcie_app, set_pcie_app: 3;
    pub pcie_lanes, set_pcie_lanes: 7, 4;
}

#[derive(
    Copy,
    Clone,
    PartialEq,
    Eq,
    From,
    FromPrimitive,
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(u32)]
// Valid values for some of the fields in the `ResetOption` register defined below. See the
// description for bits 7:0 of the register in
// 631384-0001_TF2-Top-Level_Register_Map_05062021.html.
pub enum TofinoPcieResetOptions {
    ControllerOnly = 0b00,
    ControllerAndPhyLanes = 0b10,
    EntirePhyAndController = 0b11,
}

impl From<TofinoPcieResetOptions> for u32 {
    fn from(r: TofinoPcieResetOptions) -> Self {
        r as u32
    }
}

impl From<u32> for TofinoPcieResetOptions {
    fn from(v: u32) -> Self {
        Self::from_u32(v).unwrap_or(TofinoPcieResetOptions::ControllerOnly)
    }
}

bitfield! {
    /// The `ResetOption` register allows for additional control over the reset
    /// sequence of Tofino. Note that not all fields are represented in this
    /// struct. See the full set in
    /// 631384-0001_TF2-Top-Level_Register_Map_05062021.html if additional ones
    /// are desired.
    #[derive(
        Copy,
        Clone,
        PartialEq,
        Eq,
        From,
        Into,
        FromPrimitive,
        IntoBytes,
        FromBytes,
        Immutable,
        KnownLayout
    )]
    #[repr(C)]
    pub struct ResetOptions(u32);
    // Reset the entire PHY, including a full load of SPI EEPROM contents.
    pub entire_pcie_phy, set_entire_pcie_phy: 0;
    // Reset options for several PCIe events.
    pub from into TofinoPcieResetOptions, on_pcie_link_down, set_on_pcie_link_down: 3, 2;
    pub from into TofinoPcieResetOptions, on_pcie_l2_exit, set_on_pcie_l2_exit: 5, 4;
    pub from into TofinoPcieResetOptions, on_pcie_host_reset, set_on_pcie_host_reset: 7, 6;
}

bitfield! {
    /// Control registers providing some control over the lane configuration of
    /// the PCIe PHY. Each register contains the controls for two lanes, for a
    /// total of four lanes.
    #[derive(
        Copy,
        Clone,
        PartialEq,
        Eq,
        From,
        Into,
        FromPrimitive,
        IntoBytes,
        FromBytes,
        Immutable,
        KnownLayout
    )]
    #[repr(C)]
    pub struct PciePhyLaneControl(u16);
    pub tx_to_rx_serial_loopback, set_tx_to_rx_serial_loopback: 0;
    pub rx_to_tx_parallel_loopback, set_rx_to_tx_parallel_loopback: 1;
    pub sris, set_sris: 2;
    pub rx_termination, set_rx_termination: 3;
    pub clock_pattern, set_clock_pattern: 4;
    pub pipe_tx_pattern, set_pipe_tx_pattern: 6, 5;
    pub tx_bypass_eq_calc, set_tx_bypass_eq_calc: 7;
    pub common_refclk, set_common_refclk_mode: 8;
    pub elastic_buffer_empty, set_elastic_buffer_empty: 9;
}

bitfield! {
    #[derive(
        Copy,
        Clone,
        PartialEq,
        Eq,
        From,
        Into,
        FromPrimitive,
        IntoBytes,
        FromBytes,
        Immutable,
        KnownLayout
    )]
    #[repr(C)]
    pub struct PciePhyLaneControlPair(u32);
    pub u16, into PciePhyLaneControl, lane0, set_lane0: 15, 0;
    pub u16, into PciePhyLaneControl, lane1, set_lane1: 31, 16;
}

bitfield! {
    /// Similar to the control registers above a PCIe PHY Lane Status register
    /// allows monitoring some state while the PHY is running.
    #[derive(
        Copy,
        Clone,
        PartialEq,
        Eq,
        From,
        Into,
        FromPrimitive,
        IntoBytes,
        FromBytes,
        Immutable,
        KnownLayout
    )]
    #[repr(C)]
    pub struct PciePhyLaneStatus(u16);
    pub elastic_buffer_pointer, _: 7, 0;
    pub aligned, _: 8;
    pub pipe_data_bus_width, _: 11, 10;
    pub pipe_max_p_clk, _: 13, 12;
}

bitfield! {    #[derive(
    Copy,
    Clone,
    PartialEq,
    Eq,
    From,
    Into,
    FromPrimitive,
    IntoBytes,
    FromBytes,
    Immutable,
    KnownLayout
)]
    #[repr(C)]
    pub struct PciePhyLaneStatusPair(u32);
    pub u16, into PciePhyLaneStatus, lane0, set_lane0: 15, 0;
    pub u16, into PciePhyLaneStatus, lane1, set_lane1: 31, 16;
}

bitfield! {
    /// The PCIe Controller Control register, allowing some control over high
    /// level features of the PCIe Controller. This register is not documented
    /// and Intel (or rather their IP vendor) refers to this as k_gen. See IPS
    /// 00781992 for more context.
    ///
    /// Note that with the exception of SRIS and the PHY rate selectors
    /// (presumably allowing one to force a lower link speed) modifying these
    /// parameters will break the link.
    #[derive(
        Copy,
        Clone,
        PartialEq,
        Eq,
        From,
        Into,
        FromPrimitive,
        IntoBytes,
        FromBytes,
        Immutable,
        KnownLayout
    )]
    #[repr(C)]
    pub struct PcieControllerConfiguration(u32);
    pub pcie_version, _: 3, 0;
    pub port_type, _: 7, 4;
    pub sris, set_sris: 8;
    pub rate_2_5g_supported, _: 9;
    pub rate_5g_supported, _: 10;
    pub rate_8g_supported, _: 11;
    pub rate_16g_supported, _: 12;
    pub rate_32g_supported, _: 13;
}

/// SPI EEPROM instructions, as per for example
/// https://octopart.com/datasheet/cat25512vi-gt3-onsemi-22302617.
#[derive(
    Copy, Clone, PartialEq, Eq, FromPrimitive, IntoBytes, Immutable, KnownLayout,
)]
#[repr(u8)]
pub enum SpiEepromInstruction {
    // WREN, enable write operations
    WriteEnable = 0x6,
    // WRDI, disable write operations
    WriteDisable = 0x4,
    // RDSR, read Status register
    ReadStatusRegister = 0x5,
    // WRSR, write Status register. See datasheet for which bits can actually be
    // written.
    WriteStatusRegister = 0x1,
    // READ, read a number of bytes from the EEPROM
    Read = 0x3,
    // WRITE, write a number of bytes to the EEPROM
    Write = 0x2,
}

impl From<SpiEepromInstruction> for u8 {
    fn from(i: SpiEepromInstruction) -> Self {
        i as u8
    }
}

pub struct DebugPort {
    fpga: FpgaUserDesign,
}

impl DebugPort {
    pub fn new(task_id: userlib::TaskId) -> Self {
        Self {
            fpga: FpgaUserDesign::new(
                task_id,
                MainboardController::DEVICE_INDEX,
            ),
        }
    }

    pub fn state(&self) -> Result<DebugPortState, FpgaError> {
        self.fpga.read(Addr::TOFINO_DEBUG_PORT_STATE)
    }

    /// Resets debug port state by clearing the send and receive buffers
    pub fn reset(&self) -> Result<(), FpgaError> {
        let mut state = DebugPortState(0);
        state.set_send_buffer_empty(true);
        state.set_receive_buffer_empty(true);
        self.set_state(state)
    }

    pub fn set_state(&self, state: DebugPortState) -> Result<(), FpgaError> {
        self.fpga
            .write(WriteOp::Write, Addr::TOFINO_DEBUG_PORT_STATE, state)
    }

    pub fn read_direct(
        &self,
        segment: DirectBarSegment,
        offset: impl Into<u32> + Copy,
    ) -> Result<u32, FpgaError> {
        assert!(offset.into() < 1 << 28);

        let state = self.state()?;
        if !state.send_buffer_empty() || !state.receive_buffer_empty() {
            return Err(FpgaError::InvalidState);
        }

        // Add the segement base address to the given read offset.
        let address = segment as u32 | offset.into();

        // Write the opcode.
        self.fpga.write(
            WriteOp::Write,
            Addr::TOFINO_DEBUG_PORT_BUFFER,
            DebugRequestOpcode::DirectRead,
        )?;

        // Write the address. This is done in a loop because the SPI peripheral
        // in the FPGA auto-increments an address pointer. This will be
        // refactored when a non auto-incrementing `WriteOp` is implemented.
        for b in address.as_bytes().iter() {
            self.fpga.write(
                WriteOp::Write,
                Addr::TOFINO_DEBUG_PORT_BUFFER,
                *b,
            )?;
        }

        // Start the request.
        self.fpga.write(
            WriteOp::Write,
            Addr::TOFINO_DEBUG_PORT_STATE,
            Reg::TOFINO_DEBUG_PORT_STATE::REQUEST_IN_PROGRESS,
        )?;

        // Wait for the request to complete.
        while self.state()?.request_in_progress() {
            userlib::hl::sleep_for(1);
        }

        // Read the response. This is done in a loop because the SPI peripheral
        // in the FPGA auto-increments an address pointer. This will be
        // refactored when a non auto-incrementing `read(..)` is implemented.
        let mut v: u32 = 0;
        for b in v.as_mut_bytes().iter_mut() {
            *b = self.fpga.read(Addr::TOFINO_DEBUG_PORT_BUFFER)?;
        }

        Ok(v)
    }

    pub fn write_direct(
        &self,
        segment: DirectBarSegment,
        offset: impl Into<u32> + Copy,
        value: impl Into<u32>,
    ) -> Result<(), FpgaError> {
        assert!(offset.into() < 1 << 28);

        let state = self.state()?;
        if !state.send_buffer_empty() || !state.receive_buffer_empty() {
            return Err(FpgaError::InvalidState);
        }

        // Add the segement base address to the given read offset.
        let address = segment as u32 | offset.into();

        // Write the opcode to the queue.
        self.fpga.write(
            WriteOp::Write,
            Addr::TOFINO_DEBUG_PORT_BUFFER,
            DebugRequestOpcode::DirectWrite,
        )?;

        // Write the address to the queue. This is done in a loop because the
        // SPI peripheral in the FPGA auto-increments an address pointer. This
        // will be refactored when a non auto-incrementing `WriteOp` is
        // implemented.
        for b in address.as_bytes().iter() {
            self.fpga.write(
                WriteOp::Write,
                Addr::TOFINO_DEBUG_PORT_BUFFER,
                *b,
            )?;
        }

        // Write the value to the queue.
        for b in value.into().as_bytes().iter() {
            self.fpga.write(
                WriteOp::Write,
                Addr::TOFINO_DEBUG_PORT_BUFFER,
                *b,
            )?;
        }

        // Start the request.
        self.fpga.write(
            WriteOp::Write,
            Addr::TOFINO_DEBUG_PORT_STATE,
            Reg::TOFINO_DEBUG_PORT_STATE::REQUEST_IN_PROGRESS,
        )?;

        // Wait for the request to complete.
        while self.state()?.request_in_progress() {
            userlib::hl::sleep_for(1);
        }

        Ok(())
    }

    /// Generate the SPI command Tofino needs to complete a SPI request.
    fn spi_command(n_bytes_to_write: usize, n_bytes_to_read: usize) -> u32 {
        assert!(n_bytes_to_write <= 8);
        assert!(n_bytes_to_read <= 4);

        (0x80 | ((n_bytes_to_read & 0x7) << 4) | (n_bytes_to_write & 0xf))
            .try_into()
            .unwrap()
    }

    /// Wait for a SPI request to complete.
    fn await_spi_request_done(&self) -> Result<(), FpgaError> {
        while self.read_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiCommand,
        )? & 0x80
            != 0
        {
            userlib::hl::sleep_for(1);
        }

        Ok(())
    }

    /// Send an instruction to the Tofino attached SPI EEPROM.
    pub fn send_spi_eeprom_instruction(
        &self,
        i: SpiEepromInstruction,
    ) -> Result<(), FpgaError> {
        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiOutData0,
            i as u32,
        )?;
        // Initiate the SPI transaction.
        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiCommand,
            Self::spi_command(1, 1),
        )?;

        self.await_spi_request_done()
    }

    /// Read the register containing the IDCODE latched by Tofino when it
    /// successfully reads the PCIe SerDes parameters from the SPI EEPROM.
    pub fn spi_eeprom_idcode(&self) -> Result<u32, FpgaError> {
        self.read_direct(DirectBarSegment::Bar0, TofinoBar0Registers::SpiIdCode)
    }

    /// Read the SPI EEPROM Status register.
    pub fn spi_eeprom_status(&self) -> Result<u8, FpgaError> {
        self.send_spi_eeprom_instruction(
            SpiEepromInstruction::ReadStatusRegister,
        )?;

        // Read the EEPROM response.
        Ok(self.read_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiInData,
        )? as u8)
    }

    /// Write the SPI EEPROM Status register.
    pub fn set_spi_eeprom_status(&self, value: u8) -> Result<(), FpgaError> {
        // Request the WRSR instruction with the given value.
        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiOutData0,
            u32::from_le_bytes([
                SpiEepromInstruction::WriteStatusRegister as u8,
                value,
                0,
                0,
            ]),
        )?;
        // Initiate the SPI transaction.
        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiCommand,
            Self::spi_command(1, 0),
        )?;

        self.await_spi_request_done()
    }

    /// Read four bytes from the SPI EEPROM at the given offset.
    pub fn read_spi_eeprom(&self, offset: usize) -> Result<[u8; 4], FpgaError> {
        // Request a read of the given address.
        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiOutData0,
            u32::from_le_bytes([
                SpiEepromInstruction::Read as u8,
                (offset >> 8) as u8,
                offset as u8,
                0,
            ]),
        )?;

        // Initiate the SPI transaction.
        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiCommand,
            Self::spi_command(3, 4),
        )?;

        self.await_spi_request_done()?;

        // Read the EEPROM response.
        Ok(self
            .read_direct(
                DirectBarSegment::Bar0,
                TofinoBar0Registers::SpiInData,
            )?
            .to_be_bytes())
    }

    /// Write four bytes into the SPI EEPROM at the given offset.
    pub fn write_spi_eeprom(
        &self,
        offset: usize,
        data: [u8; 4],
    ) -> Result<(), FpgaError> {
        self.send_spi_eeprom_instruction(SpiEepromInstruction::WriteEnable)?;

        // Request a Write of the given address.
        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiOutData0,
            u32::from_le_bytes([
                SpiEepromInstruction::Write as u8,
                (offset >> 8) as u8,
                offset as u8,
                data[0],
            ]),
        )?;

        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiOutData1,
            u32::from_le_bytes([data[1], data[2], data[3], 0]),
        )?;

        // Initiate the SPI transaction.
        self.write_direct(
            DirectBarSegment::Bar0,
            TofinoBar0Registers::SpiCommand,
            Self::spi_command(7, 0),
        )?;

        self.await_spi_request_done()
    }

    /// Read the requested number of bytes from the SPI EEPROM at the given
    /// offset into the given byte buffer. Note that the given offset needs to
    /// be four-byte aligned.
    pub fn read_spi_eeprom_bytes(
        &self,
        offset: usize,
        data: &mut [u8],
    ) -> Result<(), FpgaError> {
        // Only 4 byte aligned reads/writes are allowed.
        if !offset.is_multiple_of(4) {
            return Err(FpgaError::InvalidValue);
        }

        for (i, chunk) in data.chunks_mut(4).enumerate() {
            self.read_spi_eeprom(offset + (i * 4))
                .map(|bytes| chunk.copy_from_slice(&bytes[0..chunk.len()]))?;
        }

        Ok(())
    }

    /// Write the contents of the given byte buffer into the SPI EEPROM at the
    /// given offset. Note that the given offset needs to be four-byte aligned.
    pub fn write_spi_eeprom_bytes(
        &self,
        offset: usize,
        data: &[u8],
    ) -> Result<(), FpgaError> {
        // Only 4 byte aligned reads/writes are allowed.
        if !offset.is_multiple_of(4) {
            return Err(FpgaError::InvalidValue);
        }

        let mut bytes = [0u8; 4];
        for (i, chunk) in data.chunks(4).enumerate() {
            bytes[0..chunk.len()].copy_from_slice(chunk);
            self.write_spi_eeprom(offset + (i * 4), bytes)?;
        }

        Ok(())
    }
}
