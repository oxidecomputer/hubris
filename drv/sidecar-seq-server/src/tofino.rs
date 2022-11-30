// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::*;
use drv_i2c_devices::raa229618::Raa229618;
use drv_sidecar_mainboard_controller::tofino2::{DebugPort, Sequencer};

pub(crate) struct Tofino {
    pub policy: TofinoSequencerPolicy,
    pub sequencer: Sequencer,
    pub debug_port: DebugPort,
    pub vddcore: Raa229618,
    pub sequencing_error_logged: bool,
}

impl Tofino {
    pub fn new(i2c_task: userlib::TaskId) -> Self {
        let (i2c_device, rail) = i2c_config::pmbus::v0p8_tf2_vdd_core(i2c_task);
        let vddcore = Raa229618::new(&i2c_device, rail);
        Self {
            policy: TofinoSequencerPolicy::Disabled,
            sequencer: Sequencer::new(MAINBOARD.get_task_id()),
            debug_port: DebugPort::new(MAINBOARD.get_task_id()),
            vddcore,
            sequencing_error_logged: false,
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
        self.sequencing_error_logged = false;
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

            // Set Vout according to the VID and acknowledge the change to the
            // sequencer.
            if let Some(vid) = maybe_vid {
                self.apply_vid(vid)?;
                self.sequencer.ack_vid()?;
                ringbuf_entry!(Trace::TofinoVidAck);

                // Release PCIe reset, wait 200ms for the PCIe SerDes parameters
                // to load and the peripheral to initialize, and log the latched
                // IDCODE.
                self.sequencer.set_pcie_reset(TofinoPcieReset::Deasserted)?;
                hl::sleep_for(200);
                ringbuf_entry!(Trace::TofinoEepromIdCode(
                    self.debug_port.spi_eeprom_idcode()?
                ));

                // Set PCIe present to trigger a hotplug event on the attached
                // host.
                self.set_pcie_present(true)?;

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

    fn report_power_rail_aborted(&mut self, rail: &PowerRail) {
        ringbuf_entry!(Trace::TofinoPowerRailAbort(rail.id, rail.pin_state));

        if rail.pin_state.fault {
            // TODO (arjen): pull PMBus for additional data in
            // the case of faults.
        }
    }

    pub fn handle_tick(&mut self) -> Result<(), SeqError> {
        let state = self.sequencer.state()?;
        let error = self.sequencer.error()?;

        if error != TofinoSeqError::None && !self.sequencing_error_logged {
            let error_step = self.sequencer.error_step()?;
            let power_rails =
                PowerRail::from_data(self.sequencer.power_rails()?)
                    .map_err(|_| SeqError::FpgaError)?;

            self.sequencing_error_logged = true;
            ringbuf_entry!(Trace::TofinoSequencerFault(error_step, error));

            for rail in &power_rails {
                match rail.state {
                    PowerRailState::GoodTimeout => {
                        ringbuf_entry!(Trace::TofinoPowerRailGoodTimeout(
                            rail.id
                        ));
                    }
                    PowerRailState::Aborted => {
                        self.report_power_rail_aborted(rail);
                    }
                    _ => {}
                }
            }
        } else {
            ringbuf_entry!(Trace::TofinoSequencerTick(
                self.policy,
                state,
                error
            ));
        }

        match (self.policy, state, error) {
            // Power down if Tofino should be disabled.
            (
                TofinoSequencerPolicy::Disabled,
                TofinoSeqState::InPowerUp | TofinoSeqState::A0,
                _,
            ) => self.power_down(),
            // Power up
            (
                TofinoSequencerPolicy::LatchOffOnFault,
                TofinoSeqState::A2,
                TofinoSeqError::None,
            ) => self.power_up(),

            // RestartOnFault not yet implemented because we do not yet know how
            // this should behave. And we probably still want to see/debug if a
            // fault occurs and restart manually.

            // Do not change the state.
            _ => Ok(()),
        }
    }
}
