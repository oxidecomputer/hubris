// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr, MainboardController};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use drv_ignition_api::*;
use zerocopy::{AsBytes, FromBytes};

pub struct IgnitionController {
    fpga: FpgaUserDesign,
}

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
    fn port_addr(&self, port: u8, offset: IgnitionAddr) -> u16 {
        u16::from(Addr::IGNITION_CONTROLLERS_COUNT)
            + 0x100
            + (0x100 * port as u16)
            + u16::from(offset)
    }

    #[inline]
    fn read_port_register<T>(
        &self,
        port: u8,
        offset: IgnitionAddr,
    ) -> Result<T, FpgaError>
    where
        T: AsBytes + FromBytes,
    {
        self.fpga.read(self.port_addr(port, offset))
    }

    #[inline]
    fn write_port_register<T>(
        &self,
        port: u8,
        offset: IgnitionAddr,
        value: T,
    ) -> Result<(), FpgaError>
    where
        T: AsBytes + FromBytes,
    {
        self.fpga
            .write(WriteOp::Write, self.port_addr(port, offset), value)
    }

    /// Return the number of ports exposed by the Controller.
    pub fn port_count(&self) -> Result<u8, FpgaError> {
        self.fpga.read(Addr::IGNITION_CONTROLLERS_COUNT)
    }

    /// Return a bit-vector indicating Target presence on each of the Controller
    /// ports.
    pub fn presence_summary(&self) -> Result<u64, FpgaError> {
        self.fpga.read(Addr::IGNITION_TARGETS_PRESENT0)
    }

    /// Return the state for the given port.
    pub fn state(&self, port: u8) -> Result<PortState, FpgaError> {
        self.read_port_register(port, IgnitionAddr::CONTROLLER_STATUS)
            .map(PortState)
    }

    /// Return the high level counters for the given port.
    pub fn counters(&self, port: u8) -> Result<Counters, FpgaError> {
        self.read_port_register(
            port,
            IgnitionAddr::CONTROLLER_STATUS_RECEIVED_COUNT,
        )
    }

    #[inline]
    fn link_events_addr(txr: TransceiverSelect) -> IgnitionAddr {
        match txr {
            TransceiverSelect::Controller => {
                IgnitionAddr::CONTROLLER_LINK_EVENTS_SUMMARY
            }
            TransceiverSelect::TargetLink0 => {
                IgnitionAddr::TARGET_LINK0_EVENTS_SUMMARY
            }
            TransceiverSelect::TargetLink1 => {
                IgnitionAddr::TARGET_LINK1_EVENTS_SUMMARY
            }
        }
    }

    /// Return the event summary vector for the given port and link.
    pub fn link_events(
        &self,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<LinkEvents, FpgaError> {
        self.read_port_register(port, Self::link_events_addr(txr))
    }

    /// Clear the events for the given port, link.
    pub fn clear_link_events(
        &self,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<(), FpgaError> {
        self.write_port_register(
            port,
            Self::link_events_addr(txr),
            LinkEvents::ALL,
        )
    }

    /// Read the request register for the given port.
    pub fn request(&self, port: u8) -> Result<u8, FpgaError> {
        self.read_port_register(port, IgnitionAddr::TARGET_REQUEST)
    }

    /// Set the request register for the given port.
    pub fn set_request(
        &self,
        port: u8,
        request: Request,
    ) -> Result<(), FpgaError> {
        self.write_port_register(
            port,
            IgnitionAddr::TARGET_REQUEST,
            u8::from(request),
        )
    }
}

// The generated page local register map for Ignition clashes with the crate
// `Addr` type so include it in a submodule.
mod ignition_addr {
    include!(concat!(env!("OUT_DIR"), "/ignition_controller.rs"));
}

use ignition_addr::Addr as IgnitionAddr;
