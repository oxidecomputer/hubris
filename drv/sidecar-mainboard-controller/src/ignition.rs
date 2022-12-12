// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Addr as MainboardControllerAddr, MainboardController};
use drv_fpga_api::{FpgaError, FpgaUserDesign, WriteOp};
use drv_ignition_api::{Addr as IgnitionPageAddr, *};
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
    fn port_addr(&self, port: u8, offset: Addr) -> u16 {
        u16::from(MainboardControllerAddr::IGNITION_CONTROLLERS_COUNT)
            + 0x100
            + (0x100 * u16::from(port))
            + u16::from(offset)
    }

    #[inline]
    fn read_port_register<T>(
        &self,
        port: u8,
        offset: IgnitionPageAddr,
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
        offset: IgnitionPageAddr,
        value: T,
    ) -> Result<(), FpgaError>
    where
        T: AsBytes + FromBytes,
    {
        self.fpga
            .write(WriteOp::Write, self.port_addr(port, offset), value)
    }

    /// Return the number of ports exposed by the Controller.
    #[inline]
    pub fn port_count(&self) -> Result<u8, FpgaError> {
        self.fpga
            .read(MainboardControllerAddr::IGNITION_CONTROLLERS_COUNT)
    }

    /// Return a bit-vector indicating Target presence on each of the Controller
    /// ports.
    #[inline]
    pub fn presence_summary(&self) -> Result<u64, FpgaError> {
        self.fpga
            .read(MainboardControllerAddr::IGNITION_TARGETS_PRESENT0)
    }

    /// Return the state for the given port.
    #[inline]
    pub fn port_state(&self, port: u8) -> Result<PortState, FpgaError> {
        self.read_port_register::<u64>(
            port,
            IgnitionPageAddr::CONTROLLER_STATUS,
        )
        .map(PortState::from)
    }

    /// Return the high level counters for the given port.
    #[inline]
    pub fn counters(&self, port: u8) -> Result<Counters, FpgaError> {
        self.read_port_register::<[u8; 4]>(
            port,
            IgnitionPageAddr::CONTROLLER_STATUS_RECEIVED_COUNT,
        )
        .map(Counters::from)
    }

    #[inline]
    const fn transceiver_events_addr(
        txr: TransceiverSelect,
    ) -> IgnitionPageAddr {
        match txr {
            TransceiverSelect::Controller => {
                IgnitionPageAddr::CONTROLLER_LINK_EVENTS_SUMMARY
            }
            TransceiverSelect::TargetLink0 => {
                IgnitionPageAddr::TARGET_LINK0_EVENTS_SUMMARY
            }
            TransceiverSelect::TargetLink1 => {
                IgnitionPageAddr::TARGET_LINK1_EVENTS_SUMMARY
            }
        }
    }

    /// Return the event summary vector for the given port and link.
    #[inline]
    pub fn transceiver_events(
        &self,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<u8, FpgaError> {
        self.read_port_register(port, Self::transceiver_events_addr(txr))
    }

    /// Clear the events for the given port, link.
    #[inline]
    pub fn clear_transceiver_events(
        &self,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<(), FpgaError> {
        self.write_port_register(
            port,
            Self::transceiver_events_addr(txr),
            u8::from(TransceiverEvents::ALL),
        )
    }

    /// Read the request register for the given port.
    #[inline]
    pub fn request(&self, port: u8) -> Result<u8, FpgaError> {
        self.read_port_register(port, IgnitionPageAddr::TARGET_REQUEST)
    }

    /// Set the request register for the given port.
    #[inline]
    pub fn set_request(
        &self,
        port: u8,
        request: Request,
    ) -> Result<(), FpgaError> {
        self.write_port_register(
            port,
            IgnitionPageAddr::TARGET_REQUEST,
            u8::from(request),
        )
    }
}
