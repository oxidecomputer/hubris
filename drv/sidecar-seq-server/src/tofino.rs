// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::*;
use drv_i2c_devices::raa229618::Raa229618;
use drv_sidecar_mainboard_controller::tofino2::Sequencer;

pub(crate) struct Tofino {
    pub policy: TofinoSequencerPolicy,
    pub sequencer: Sequencer,
    pub vddcore: Raa229618,
}

impl Tofino {
    pub fn new(i2c_task: userlib::TaskId) -> Self {
        let (i2c_device, rail) = i2c_config::pmbus::v0p8_tf2_vdd_core(i2c_task);
        let vddcore = Raa229618::new(&i2c_device, rail);
        Self {
            policy: TofinoSequencerPolicy::Disabled,
            sequencer: Sequencer::new(MAINBOARD.get_task_id()),
            vddcore,
        }
    }

    pub fn apply_vid(&mut self, vid: Tofino2Vid) -> Result<(), SeqError> {
        use userlib::units::Volts;

        let value = Volts(match vid {
            Tofino2Vid::V0P922 => 0.922,
            Tofino2Vid::V0P893 => 0.893,
            Tofino2Vid::V0P867 => 0.867,
            Tofino2Vid::V0P847 => 0.847,
            Tofino2Vid::V0P831 => 0.831,
            Tofino2Vid::V0P815 => 0.815,
            Tofino2Vid::V0P790 => 0.790,
            Tofino2Vid::V0P759 => 0.759,
        });
        self.vddcore
            .set_vout(value)
            .map_err(|_| SeqError::SetVddCoreVoutFailed)?;

        ringbuf_entry!(Trace::SetVddCoreVout(value));
        Ok(())
    }

    pub fn set_pcie_present(&mut self, present: bool) -> Result<(), SeqError> {
        let entry = if present {
            Trace::SetPCIePresent
        } else {
            Trace::ClearPCIePresent
        };
        ringbuf_entry!(entry);
        self.sequencer
            .set_pcie_present(present)
            .map_err(|_| SeqError::FpgaError)
    }

    pub fn power_up(&mut self) -> Result<(), SeqError> {
        ringbuf_entry!(Trace::InitiateTofinoPowerUp);

        // Initiate the power up sequence.
        self.sequencer.set_enable(true)?;

        // Wait for the VID to become valid, retrying if needed.
        for i in 1..4 {
            // Sleep first since there is a delay between the sequencer
            // receiving the EN bit and the VID being valid.
            hl::sleep_for(i * 25);

            let maybe_vid = self.sequencer.vid().map_err(|e| {
                if let FpgaError::InvalidValue = e {
                    SeqError::InvalidTofinoVid
                } else {
                    SeqError::FpgaError
                }
            })?;

            // Set Vout accordingy to the VID and acknowledge the change to the
            // sequencer.
            if let Some(vid) = maybe_vid {
                self.apply_vid(vid)?;
                self.sequencer.ack_vid()?;
                ringbuf_entry!(Trace::TofinoVidAck);

                // Set PCIe present and reset.
                self.set_pcie_present(true)?;
                self.sequencer.set_pcie_reset(TofinoPcieReset::Deasserted)?;

                return Ok(());
            }
        }

        Err(SeqError::SequencerTimeout)
    }

    pub fn power_down(&mut self) -> Result<(), SeqError> {
        ringbuf_entry!(Trace::InitiateTofinoPowerDown);
        self.set_pcie_present(false)?;
        self.sequencer.set_pcie_reset(TofinoPcieReset::Asserted)?;
        self.sequencer
            .set_enable(false)
            .map_err(|_| SeqError::SequencerError)
    }

    pub fn handle_tick(&mut self) -> Result<(), SeqError> {
        let state = self.sequencer.state()?;
        let error = self.sequencer.error()?;

        ringbuf_entry!(Trace::TofinoSequencerTick(self.policy, state, error));

        match (self.policy, state, error) {
            // Power down if the Tofino should be disabled.
            (TofinoSequencerPolicy::Disabled, TofinoSeqState::InPowerUp, _) => {
                self.power_down()
            }
            (TofinoSequencerPolicy::Disabled, TofinoSeqState::A0, _) => {
                self.power_down()
            }
            // Power up
            (
                TofinoSequencerPolicy::LatchOffOnFault,
                TofinoSeqState::A2,
                TofinoSeqError::None,
            ) => self.power_up(),
            // RestartOnFault not yet implemented because we do not yet know how
            // this should behave. And we probably still want to see/debug if a
            // fault occurs and restart manually.
            _ => Ok(()), // Do nothing by default.
        }
    }
}
