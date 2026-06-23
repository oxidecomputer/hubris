// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::cell::Cell;
use drv_ignition_api::{
    AllLinkEventsIter, AllPortsIter, Ignition, IgnitionError,
};
use gateway_messages::ignition::{
    IgnitionState, LinkEvents, ReceiverStatus, SystemFaults, SystemPowerState,
    SystemType, TargetState, TransceiverEvents, TransceiverSelect,
};
use gateway_messages::{IgnitionCommand, SpError};
use heapless::Vec;
use userlib::UnwrapLite;

userlib::task_slot!(IGNITION, ignition);

pub(crate) struct IgnitionController {
    task: Ignition,
    // We cache the number of ignition ports the first time we successfully call
    // it since it never changes (it's the total number of ports, which is baked
    // into the FPGA image, not the number of present targets, which varies at
    // runtime).
    num_ports: Cell<Option<u32>>,
}

impl IgnitionController {
    pub(crate) fn new() -> Self {
        Self {
            task: Ignition::new(IGNITION.get_task_id()),
            num_ports: Cell::new(None),
        }
    }

    pub(crate) fn num_ports(&self) -> Result<u32, SpError> {
        if let Some(n) = self.num_ports.get() {
            return Ok(n);
        }

        let n = u32::from(
            self.task
                .port_count()
                .map_err(sp_error_from_ignition_error)?,
        );
        self.num_ports.set(Some(n));
        Ok(n)
    }

    pub(crate) fn target_state(
        &self,
        target: u8,
    ) -> Result<IgnitionState, SpError> {
        let port = self
            .task
            .port(target)
            .map_err(sp_error_from_ignition_error)?;
        Ok(PortConvert(port).into())
    }

    pub(crate) fn bulk_state(
        &self,
        offset: u32,
    ) -> Result<BulkIgnitionStateIter, SpError> {
        let iter = self
            .task
            .all_ports()
            .map_err(sp_error_from_ignition_error)?;
        Ok(BulkIgnitionStateIter {
            iter: iter.skip(offset as usize),
        })
    }

    pub(crate) fn target_link_events(
        &self,
        target: u8,
    ) -> Result<LinkEvents, SpError> {
        let events = self
            .task
            .link_events(target)
            .map_err(sp_error_from_ignition_error)?;
        Ok(LinkEventsConvert(events).into())
    }

    pub(crate) fn bulk_link_events(
        &self,
        offset: u32,
    ) -> Result<BulkIgnitionLinkEventsIter, SpError> {
        let iter = self
            .task
            .all_link_events()
            .map_err(sp_error_from_ignition_error)?;
        Ok(BulkIgnitionLinkEventsIter {
            iter: iter.skip(offset as usize),
        })
    }

    pub(super) fn clear_link_events(
        &self,
        target: Option<u8>,
        transceiver_select: Option<TransceiverSelect>,
    ) -> Result<(), SpError> {
        use drv_ignition_api::TransceiverSelect as IgnitionTxrSelect;

        // Convert `target` to a range (either of length 1, if we got a target,
        // or for all targets).
        let targets = match target {
            Some(t) => t..t + 1,
            None => 0..self.num_ports()? as u8,
        };

        // Convert `transceiver_select` into a vec of at most 3 items (all
        // transceivers if we didn't get one, or 1 if we did).
        let mut txrs = Vec::<_, 3>::new();
        match transceiver_select {
            Some(TransceiverSelect::Controller) => {
                txrs.push(IgnitionTxrSelect::Controller).unwrap_lite();
            }
            Some(TransceiverSelect::TargetLink0) => {
                txrs.push(IgnitionTxrSelect::TargetLink0).unwrap_lite();
            }
            Some(TransceiverSelect::TargetLink1) => {
                txrs.push(IgnitionTxrSelect::TargetLink1).unwrap_lite();
            }
            None => {
                txrs.push(IgnitionTxrSelect::Controller).unwrap_lite();
                txrs.push(IgnitionTxrSelect::TargetLink0).unwrap_lite();
                txrs.push(IgnitionTxrSelect::TargetLink1).unwrap_lite();
            }
        }

        // Clear all requested events (at least 1, at most num_ports * 3).
        //
        // We fail on the first error here; is that reasonable? Should we return
        // as part of the error how far we got? If the caller cares at that
        // level, is it sufficient for them to be able to call us separately for
        // each target/transceiver they care about?
        for target in targets {
            for &txr in &txrs {
                self.task
                    .clear_transceiver_events(target, txr)
                    .map_err(sp_error_from_ignition_error)?;
            }
        }

        Ok(())
    }

    pub(super) fn command(
        &self,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), SpError> {
        use drv_ignition_api::Request;
        let cmd = match command {
            // We intercept the AlwaysTransmit command as it is not part of the
            // Ignition protocol (not something we send to a target), it is
            // a setting in the controller itself.
            IgnitionCommand::AlwaysTransmit { enabled } => {
                return self
                    .task
                    .set_always_transmit(target, enabled)
                    .map_err(sp_error_from_ignition_error);
            }
            IgnitionCommand::PowerOn => Request::SystemPowerOn,
            IgnitionCommand::PowerOff => Request::SystemPowerOff,
            IgnitionCommand::PowerReset => Request::SystemPowerReset,
        };
        self.task
            .send_request(target, cmd)
            .map_err(sp_error_from_ignition_error)?;

        Ok(())
    }
}

pub(crate) struct BulkIgnitionStateIter {
    iter: core::iter::Skip<AllPortsIter>,
}

impl Iterator for BulkIgnitionStateIter {
    type Item = IgnitionState;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|state| PortConvert(state).into())
    }
}

pub(crate) struct BulkIgnitionLinkEventsIter {
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
    fn from(t: TargetConvert) -> Self {
        let TargetConvert(t) = t;
        Self {
            system_type: SystemType::from(u16::from(t.id.0)),
            power_state: SystemPowerStateConvert(t.power_state).into(),
            power_reset_in_progress: t.power_reset_in_progress,
            faults: SystemFaultsConvert(t.faults).into(),
            controller0_present: t.controller0_present,
            controller1_present: t.controller1_present,
            link0_receiver_status: ReceiverStatusConvert(
                t.link0_receiver_status,
            )
            .into(),
            link1_receiver_status: ReceiverStatusConvert(
                t.link1_receiver_status,
            )
            .into(),
        }
    }
}

struct SystemPowerStateConvert(drv_ignition_api::SystemPowerState);

impl From<SystemPowerStateConvert> for SystemPowerState {
    fn from(s: SystemPowerStateConvert) -> Self {
        use drv_ignition_api::SystemPowerState as Sps;
        match s.0 {
            Sps::Off => Self::Off,
            Sps::On => Self::On,
            Sps::Aborted => Self::Aborted,
            Sps::PoweringOff => Self::PoweringOff,
            Sps::PoweringOn => Self::PoweringOn,
        }
    }
}

struct SystemFaultsConvert(drv_ignition_api::SystemFaults);

impl From<SystemFaultsConvert> for SystemFaults {
    fn from(s: SystemFaultsConvert) -> Self {
        Self {
            power_a3: s.0.power_a3,
            power_a2: s.0.power_a2,
            sp: s.0.sp,
            rot: s.0.rot,
        }
    }
}

struct LinkEventsConvert(drv_ignition_api::LinkEvents);

impl From<LinkEventsConvert> for LinkEvents {
    fn from(e: LinkEventsConvert) -> Self {
        Self {
            controller: TransceiverEventsConvert(e.0.controller).into(),
            target_link0: TransceiverEventsConvert(e.0.target_link0).into(),
            target_link1: TransceiverEventsConvert(e.0.target_link1).into(),
        }
    }
}

struct TransceiverEventsConvert(drv_ignition_api::TransceiverEvents);

impl From<TransceiverEventsConvert> for TransceiverEvents {
    fn from(e: TransceiverEventsConvert) -> Self {
        let TransceiverEventsConvert(e) = e;
        Self {
            encoding_error: e.encoding_error,
            decoding_error: e.decoding_error,
            ordered_set_invalid: e.ordered_set_invalid,
            message_version_invalid: e.message_version_invalid,
            message_type_invalid: e.message_type_invalid,
            message_checksum_invalid: e.message_checksum_invalid,
        }
    }
}

// Helper function for `.map_err()`; we can't use `?` because we can't implement
// `From<_>` between these types due to orphan rules.
fn sp_error_from_ignition_error(err: IgnitionError) -> SpError {
    use gateway_messages::ignition::IgnitionError as E;
    let err = match err {
        IgnitionError::FpgaError => E::FpgaError,
        IgnitionError::InvalidPort => E::InvalidPort,
        IgnitionError::InvalidValue => E::InvalidValue,
        IgnitionError::NoTargetPresent => E::NoTargetPresent,
        IgnitionError::RequestInProgress => E::RequestInProgress,
        IgnitionError::RequestDiscarded => E::RequestDiscarded,
        _ => E::Other(err as u32),
    };
    SpError::Ignition(err)
}
