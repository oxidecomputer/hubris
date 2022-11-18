// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use gateway_messages::ignition::{
    IgnitionState, LinkEvents, ReceiverStatus, SystemFaults, SystemPowerState,
    SystemType, Target, TransceiverEvents, TransceiverSelect,
};
use gateway_messages::SpError;

pub(super) struct IgnitionController;

const FAKE_NUM_PORTS: u32 = 350;

impl IgnitionController {
    pub(super) fn num_ignition_ports(&self) -> Result<u32, SpError> {
        Ok(FAKE_NUM_PORTS)
    }

    pub(super) fn ignition_state(
        &mut self,
        target: u8,
    ) -> Result<IgnitionState, SpError> {
        Ok(fake_ignition_state(target))
    }

    pub(super) fn bulk_ignition_state(
        &mut self,
        offset: u32,
    ) -> Result<BulkIgnitionStateIter, SpError> {
        Ok(BulkIgnitionStateIter { offset })
    }

    pub(super) fn ignition_link_events(
        &mut self,
        target: u8,
    ) -> Result<LinkEvents, SpError> {
        Ok(fake_all_link_events(target))
    }

    pub(super) fn bulk_ignition_link_events(
        &mut self,
        offset: u32,
    ) -> Result<BulkIgnitionLinkEventsIter, SpError> {
        Ok(BulkIgnitionLinkEventsIter { offset })
    }

    pub(super) fn clear_ignition_link_events(
        &mut self,
        _target: Option<u8>,
        _transceiver_select: Option<TransceiverSelect>,
    ) -> Result<(), SpError> {
        Ok(())
    }
}

fn fake_ignition_state(target: u8) -> IgnitionState {
    IgnitionState {
        receiver_status: fake_receiver_status(),
        target: if target < 5 {
            None
        } else {
            Some(fake_target())
        },
    }
}

fn fake_receiver_status() -> ReceiverStatus {
    ReceiverStatus {
        aligned: true,
        locked: true,
        polarity_inverted: false,
    }
}

fn fake_target() -> Target {
    Target {
        system_type: SystemType::Gimlet,
        power_state: SystemPowerState::On,
        power_reset_in_progress: false,
        faults: SystemFaults {
            power_a3: false,
            power_a2: false,
            sp: false,
            rot: false,
        },
        controller0_present: true,
        controller1_present: true,
        link0_receiver_status: fake_receiver_status(),
        link1_receiver_status: fake_receiver_status(),
    }
}

pub struct BulkIgnitionStateIter {
    offset: u32,
}

impl Iterator for BulkIgnitionStateIter {
    type Item = IgnitionState;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset < FAKE_NUM_PORTS {
            let s = fake_ignition_state(self.offset as u8);
            self.offset += 1;
            Some(s)
        } else {
            None
        }
    }
}

fn fake_transceiver_events(encoding_error: bool) -> TransceiverEvents {
    TransceiverEvents {
        encoding_error,
        decoding_error: false,
        ordered_set_invalid: false,
        message_version_invalid: false,
        message_type_invalid: false,
        message_checksum_invalid: false,
    }
}

fn fake_all_link_events(target: u8) -> LinkEvents {
    let encoding_error = target < 10;
    LinkEvents {
        controller: fake_transceiver_events(encoding_error),
        target_link0: fake_transceiver_events(encoding_error),
        target_link1: fake_transceiver_events(encoding_error),
    }
}

pub struct BulkIgnitionLinkEventsIter {
    offset: u32,
}

impl Iterator for BulkIgnitionLinkEventsIter {
    type Item = LinkEvents;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset < FAKE_NUM_PORTS {
            let s = fake_all_link_events(self.offset as u8);
            self.offset += 1;
            Some(s)
        } else {
            None
        }
    }
}
