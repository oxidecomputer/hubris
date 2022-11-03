// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::MainboardController;
use bitfield::bitfield;
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use userlib::{FromPrimitive, ToPrimitive};
use zerocopy::{AsBytes, FromBytes};

//include!(concat!(env!("OUT_DIR"), "/ignition_controller.rs"));

pub struct IgnitionController {
    fpga: FpgaUserDesign,
    address_base: u16,
}

impl IgnitionController {
    pub fn new(task_id: userlib::TaskId, address_base: u16) -> Self {
        Self {
            fpga: FpgaUserDesign::new(
                task_id,
                MainboardController::DEVICE_INDEX,
            ),
            address_base,
        }
    }

    #[inline]
    fn addr<A>(&self, id: u8, addr: A) -> u16
    where
        u16: From<A>,
    {
        self.address_base + 0x100 + (0x100 * id as u16) + u16::from(addr)
    }

    fn read_raw<A, T>(&self, id: u8, addr: A) -> Result<T, FpgaError>
    where
        u16: From<A>,
        T: AsBytes + Default + FromBytes,
    {
        self.fpga.read(self.addr(id, addr))
    }

    fn write_raw<A, T>(
        &self,
        id: u8,
        addr: A,
        value: T,
    ) -> Result<(), FpgaError>
    where
        u16: From<A>,
        T: AsBytes + Default + FromBytes,
    {
        self.fpga.write(WriteOp::Write, self.addr(id, addr), value)
    }

    pub fn port_count(&self) -> Result<usize, FpgaError> {
        Ok(self.fpga.read::<u8>(self.address_base)? as usize)
    }

    pub fn presence_summary(&self) -> Result<u64, FpgaError> {
        Ok(self.fpga.read(self.address_base + 1)?)
    }

    pub fn state(&self, id: u8) -> Result<ControllerState, FpgaError> {
        self.read_raw::<u16, u64>(id, 0x0).map(ControllerState)
    }

    pub fn counters(&self, id: u8) -> Result<[u8; 4], FpgaError> {
        self.read_raw::<u16, [u8; 4]>(id, 0x10)
    }

    pub fn request(&self, id: u8) -> Result<u8, FpgaError> {
        self.read_raw::<u16, u8>(id, 0x8)
    }

    pub fn set_request(
        &self,
        id: u8,
        request: Request,
    ) -> Result<(), FpgaError> {
        self.write_raw::<u16, u8>(id, 0x8, request.to_u8().unwrap_or(0))
    }
}

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct ReceiverStatus(u8);
    pub aligned, _: 0;
    pub locked, _: 1;
    pub polarity_inverted, _: 2;
}

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct ControllerState(u64);
    pub target_present, _: 0;
}

impl ControllerState {
    pub fn target(&self) -> Option<Target> {
        if self.target_present() {
            Some(Target(self.0 & 0xffffffffffff0000))
        } else {
            None
        }
    }

    pub fn receiver_status(&self) -> ReceiverStatus {
        ReceiverStatus((self.0 >> 8) as u8)
    }
}

bitfield! {
    #[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, FromBytes, AsBytes)]
    #[repr(C)]
    pub struct Target(u64);
    pub controller0_present, _: 24;
    pub controller1_present, _: 25;
    pub system_power_abort, _: 27;
    pub system_power_fault_a3, _: 32;
    pub system_power_fault_a2, _: 33;
    pub reserved_fault1, _: 34;
    pub reserved_fault2, _: 35;
    pub sp_fault, _: 36;
    pub rot_fault, _: 37;
    pub system_power_off_in_progress, _: 40;
    pub system_power_on_in_progress, _: 41;
    pub system_reset_in_progress, _: 42;
}

impl Target {
    pub fn system_type(&self) -> SystemType {
        SystemType(self.0.as_bytes()[2])
    }

    pub fn system_power_state(&self) -> PowerState {
        match self.0.as_bytes()[3] & 0x4 != 0 {
            true => PowerState::On,
            false => PowerState::Off,
        }
    }

    pub fn link0_receiver_status(&self) -> ReceiverStatus {
        ReceiverStatus(self.0.as_bytes()[6])
    }

    pub fn link1_receiver_status(&self) -> ReceiverStatus {
        ReceiverStatus(self.0.as_bytes()[7])
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    FromPrimitive,
    ToPrimitive,
    FromBytes,
    AsBytes,
)]
#[repr(C)]
pub struct SystemType(pub u8);

#[derive(
    Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum PowerState {
    Off = 0,
    On = 1,
}

#[derive(
    Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum Request {
    SystemPowerOff = 1,
    SystemPowerOn = 2,
    SystemReset = 3,
}
