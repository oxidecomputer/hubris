// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::cell::Cell;
use drv_ignition_api::{
    AllLinkEventsIter, AllPortsIter, Ignition, IgnitionError,
};
use gateway_messages::ignition::{
    IgnitionState, LinkEvents, ReceiverStatus, SystemFaults, SystemPowerState,
    SystemType, TargetState, TransceiverEvents,
};

use super::IGNITION;

pub(super) struct IgnitionController {
    task: Ignition,
    num_ports: Cell<Option<u32>>,
}

impl IgnitionController {
    pub(super) fn new() -> Self {
        Self {
            task: Ignition::new(IGNITION.get_task_id()),
            num_ports: Cell::new(None),
        }
    }

    pub(super) fn num_ports(&self) -> Result<u32, IgnitionError> {
        if let Some(n) = self.num_ports.get() {
            return Ok(n);
        }

        let n = u32::from(self.task.port_count()?);
        self.num_ports.set(Some(n));
        Ok(n)
    }

    pub(super) fn target_state(
        &self,
        target: u8,
    ) -> Result<IgnitionState, IgnitionError> {
        let port = self.task.port(target)?;
        Ok(PortConvert(port).into())
    }

    pub(super) fn bulk_state(
        &self,
        offset: u32,
    ) -> Result<BulkIgnitionStateIter, IgnitionError> {
        let iter = self.task.all_ports()?;
        Ok(BulkIgnitionStateIter {
            iter: iter.skip(offset as usize),
        })
    }

    pub(super) fn target_link_events(
        &self,
        target: u8,
    ) -> Result<LinkEvents, IgnitionError> {
        let events = self.task.link_events(target)?;
        Ok(LinkEventsConvert(events).into())
    }

    pub(super) fn bulk_link_events(
        &self,
        offset: u32,
    ) -> Result<BulkIgnitionLinkEventsIter, IgnitionError> {
        let iter = self.task.all_link_events()?;
        Ok(BulkIgnitionLinkEventsIter {
            iter: iter.skip(offset as usize),
        })
    }
}

pub struct BulkIgnitionStateIter {
    iter: core::iter::Skip<AllPortsIter>,
}

impl Iterator for BulkIgnitionStateIter {
    type Item = IgnitionState;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|state| PortConvert(state).into())
    }
}

pub struct BulkIgnitionLinkEventsIter {
    iter: core::iter::Skip<AllLinkEventsIter>,
}

impl Iterator for BulkIgnitionLinkEventsIter {
    type Item = LinkEvents;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter
            .next()
            .map(|events| LinkEventsConvert(events).into())
    }
}

struct PortConvert(drv_ignition_api::Port);

impl From<PortConvert> for IgnitionState {
    fn from(port: PortConvert) -> Self {
        let PortConvert(port) = port;
        Self {
            receiver: ReceiverStatusConvert(port.receiver_status).into(),
            target: port.target.map(|t| TargetConvert(t).into()),
        }
    }
}

struct ReceiverStatusConvert(drv_ignition_api::ReceiverStatus);

impl From<ReceiverStatusConvert> for ReceiverStatus {
    fn from(s: ReceiverStatusConvert) -> Self {
        Self {
            aligned: s.0.aligned,
            locked: s.0.locked,
            polarity_inverted: s.0.polarity_inverted,
        }
    }
}

struct TargetConvert(drv_ignition_api::Target);

impl From<TargetConvert> for TargetState {
    fn from(target: TargetConvert) -> Self {
        let TargetConvert(target) = target;
        Self {
            // Minibar uses SystemId(u8), convert to SystemType via u16
            system_type: SystemType::from(u16::from(target.id.0)),
            power_state: SystemPowerStateConvert(target.power_state).into(),
            power_reset_in_progress: target.power_reset_in_progress,
            controller0_present: target.controller0_present,
            controller1_present: target.controller1_present,
            link0_receiver_status: ReceiverStatusConvert(
                target.link0_receiver_status,
            )
            .into(),
            link1_receiver_status: ReceiverStatusConvert(
                target.link1_receiver_status,
            )
            .into(),
            faults: SystemFaultsConvert(target.faults).into(),
        }
    }
}

struct SystemPowerStateConvert(drv_ignition_api::SystemPowerState);

impl From<SystemPowerStateConvert> for SystemPowerState {
    fn from(s: SystemPowerStateConvert) -> Self {
        match s.0 {
            drv_ignition_api::SystemPowerState::Off => Self::Off,
            drv_ignition_api::SystemPowerState::On => Self::On,
            drv_ignition_api::SystemPowerState::PoweringOff => Self::PoweringOff,
            drv_ignition_api::SystemPowerState::PoweringOn => Self::PoweringOn,
            drv_ignition_api::SystemPowerState::Aborted => Self::Aborted,
        }
    }
}

struct SystemFaultsConvert(drv_ignition_api::SystemFaults);

impl From<SystemFaultsConvert> for SystemFaults {
    fn from(faults: SystemFaultsConvert) -> Self {
        Self {
            power_a3: faults.0.power_a3,
            power_a2: faults.0.power_a2,
            sp: faults.0.sp,
            rot: faults.0.rot,
        }
    }
}

struct LinkEventsConvert(drv_ignition_api::LinkEvents);

impl From<LinkEventsConvert> for LinkEvents {
    fn from(events: LinkEventsConvert) -> Self {
        Self {
            controller: TransceiverEventsConvert(events.0.controller).into(),
            target_link0: TransceiverEventsConvert(events.0.target_link0).into(),
            target_link1: TransceiverEventsConvert(events.0.target_link1).into(),
        }
    }
}

struct TransceiverEventsConvert(drv_ignition_api::TransceiverEvents);

impl From<TransceiverEventsConvert> for TransceiverEvents {
    fn from(events: TransceiverEventsConvert) -> Self {
        Self {
            encoding_error: events.0.encoding_error,
            decoding_error: events.0.decoding_error,
            ordered_set_invalid: events.0.ordered_set_invalid,
            message_version_invalid: events.0.message_version_invalid,
            message_type_invalid: events.0.message_type_invalid,
            message_checksum_invalid: events.0.message_checksum_invalid,
        }
    }
}
