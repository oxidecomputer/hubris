// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::ignition_controller::IgnitionController;
use drv_ignition_api::IgnitionError;
use gateway_messages::ignition::TransceiverSelect;
use gateway_messages::IgnitionCommand;
use heapless::Vec;
use userlib::UnwrapLite;

impl IgnitionController {
    pub(super) fn clear_link_events(
        &self,
        target: Option<u8>,
        transceiver_select: Option<TransceiverSelect>,
    ) -> Result<(), IgnitionError> {
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
                self.task.clear_transceiver_events(target, txr)?;
            }
        }

        Ok(())
    }

    pub(super) fn command(
        &self,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), IgnitionError> {
        use drv_ignition_api::Request;
        let cmd = match command {
            // We intercept the AlwaysTransmit command as it is not part of the
            // Ignition protocol (not something we send to a target), it is
            // a setting in the controller itself.
            IgnitionCommand::AlwaysTransmit { enabled } => {
                return self.task.set_always_transmit(target, enabled);
            }
            IgnitionCommand::PowerOn => Request::SystemPowerOn,
            IgnitionCommand::PowerOff => Request::SystemPowerOff,
            IgnitionCommand::PowerReset => Request::SystemPowerReset,
        };
        self.task.send_request(target, cmd)?;

        Ok(())
    }
}
