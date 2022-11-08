// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::MainboardController;

use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use drv_ignition_api::*;
use zerocopy::{AsBytes, FromBytes};

//include!(concat!(env!("OUT_DIR"), "/ignition_controller.rs"));

pub struct IgnitionController {
    fpga: FpgaUserDesign,
}

const IGNITION_BASE_ADDRESS: u16 = 0x300;
const IGNITION_FIRST_PORT_BASE_ADDRESS: u16 = IGNITION_BASE_ADDRESS + 0x100;

impl IgnitionController {
    pub fn new(task_port: userlib::TaskId) -> Self {
        Self {
            fpga: FpgaUserDesign::new(
                task_port,
                MainboardController::DEVICE_INDEX,
            ),
        }
    }

    #[inline]
    fn addr<A>(&self, port: u8, offset: A) -> u16
    where
        u16: From<A>,
    {
        IGNITION_FIRST_PORT_BASE_ADDRESS
            + (0x100 * port as u16)
            + u16::from(offset)
    }

    fn read_port_register<A, T>(
        &self,
        port: u8,
        offset: A,
    ) -> Result<T, FpgaError>
    where
        u16: From<A>,
        T: AsBytes + Default + FromBytes,
    {
        self.fpga.read(self.addr(port, offset))
    }

    fn write_port_register<A, T>(
        &self,
        port: u8,
        offset: A,
        value: T,
    ) -> Result<(), FpgaError>
    where
        u16: From<A>,
        T: AsBytes + Default + FromBytes,
    {
        self.fpga
            .write(WriteOp::Write, self.addr(port, offset), value)
    }

    /// Return the number of ports exposed by the Controller.
    pub fn port_count(&self) -> Result<u8, FpgaError> {
        self.fpga.read(IGNITION_BASE_ADDRESS)
    }

    /// Return a bit-vector indicating Target presence on each of the Controller
    /// ports.
    pub fn presence_summary(&self) -> Result<u64, FpgaError> {
        self.fpga.read(IGNITION_BASE_ADDRESS + 1)
    }

    /// Return the state for the given port.
    pub fn state(&self, port: u8) -> Result<PortState, FpgaError> {
        self.read_port_register(port, 0x0u16).map(PortState)
    }

    /// Return the high level counters for the given port.
    pub fn counters(&self, port: u8) -> Result<Counters, FpgaError> {
        self.read_port_register(port, 0x10u16)
    }

    /// Read the request register for the given port.
    pub fn request(&self, port: u8) -> Result<u8, FpgaError> {
        self.read_port_register(port, 0x8u16)
    }

    /// Set the request register for the given port.
    pub fn set_request(
        &self,
        port: u8,
        request: Request,
    ) -> Result<(), FpgaError> {
        self.write_port_register(port, 0x8u16, u8::from(request))
    }
}
