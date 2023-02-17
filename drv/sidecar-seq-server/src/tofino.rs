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
    pub abort_reported: bool,
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
            abort_reported: false,
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
        self.abort_reported = false;
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

                // Keep parts of the PCIe PHY lanes in reset and delay PCIE_INIT
                // so changes to the config can be made after loading parameters
                // from EEPROM.
                let mut software_reset =
                    SoftwareReset(self.debug_port.read_direct(
                        DirectBarSegment::Bar0,
                        TofinoBar0Registers::SoftwareReset,
                    )?);

                software_reset.set_pcie_lanes(0xf); // Bit mask to select lanes.
                self.debug_port.write_direct(
                    DirectBarSegment::Bar0,
                    TofinoBar0Registers::SoftwareReset,
                    software_reset,
                )?;
                ringbuf_entry!(Trace::TofinoBar0RegisterValue(
                    TofinoBar0Registers::SoftwareReset,
                    self.debug_port.read_direct(
                        DirectBarSegment::Bar0,
                        TofinoBar0Registers::SoftwareReset
                    )?
                ));

                // Release PCIe reset, wait 200ms for the PCIe SerDes parameters
                // to load and the peripheral to initialize. Log the latched
                // IDCODE afterwards.
                self.sequencer.set_pcie_reset(TofinoPcieReset::Deasserted)?;
                hl::sleep_for(200);
                ringbuf_entry!(Trace::TofinoEepromIdCode(
                    self.debug_port.spi_eeprom_idcode()?
                ));

                // The EEPROM contents have loaded, scribble over some of the
                // registers to enable SRIS.

                let set_sris = |r| -> Result<(), SeqError> {
                    let mut pcie_lane_ctrl_pair = PciePhyLaneControlPair(
                        self.debug_port
                            .read_direct(DirectBarSegment::Bar0, r)?,
                    );
                    let mut lane0_ctrl = pcie_lane_ctrl_pair.lane0();
                    let mut lane1_ctrl = pcie_lane_ctrl_pair.lane1();

                    lane0_ctrl.set_sris(true);
                    lane1_ctrl.set_sris(true);

                    pcie_lane_ctrl_pair.set_lane0(lane0_ctrl.into());
                    pcie_lane_ctrl_pair.set_lane1(lane1_ctrl.into());

                    self.debug_port.write_direct(
                        DirectBarSegment::Bar0,
                        r,
                        pcie_lane_ctrl_pair,
                    )?;
                    ringbuf_entry!(Trace::TofinoBar0RegisterValue(
                        r,
                        self.debug_port
                            .read_direct(DirectBarSegment::Bar0, r)?
                    ));

                    Ok(())
                };

                set_sris(TofinoBar0Registers::PciePhyLaneControl0)?;
                set_sris(TofinoBar0Registers::PciePhyLaneControl1)?;

                // Enable SRIS in the controller in order to adjust the SKP
                // Ordered Sets interval, allowing the SP3 to keep up with the
                // faster 100MHz ref clock used by Tofino.
                let mut pcie_controller =
                    PcieControllerConfiguration(self.debug_port.read_direct(
                        DirectBarSegment::Cfg,
                        TofinoCfgRegisters::KGen,
                    )?);
                pcie_controller.set_sris(true);
                self.debug_port.write_direct(
                    DirectBarSegment::Cfg,
                    TofinoCfgRegisters::KGen,
                    pcie_controller,
                )?;
                ringbuf_entry!(Trace::TofinoCfgRegisterValue(
                    TofinoCfgRegisters::KGen,
                    self.debug_port.read_direct(
                        DirectBarSegment::Cfg,
                        TofinoCfgRegisters::KGen
                    )?
                ));

                // Release the PCIe PHY from reset.
                software_reset = SoftwareReset(self.debug_port.read_direct(
                    DirectBarSegment::Bar0,
                    TofinoBar0Registers::SoftwareReset,
                )?);
                software_reset.set_pcie_lanes(0);
                self.debug_port.write_direct(
                    DirectBarSegment::Bar0,
                    TofinoBar0Registers::SoftwareReset,
                    software_reset,
                )?;
                ringbuf_entry!(Trace::TofinoBar0RegisterValue(
                    TofinoBar0Registers::SoftwareReset,
                    self.debug_port.read_direct(
                        DirectBarSegment::Bar0,
                        TofinoBar0Registers::SoftwareReset
                    )?
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

    pub fn report_abort(
        &mut self,
        abort: &TofinoSeqAbort,
    ) -> Result<(), SeqError> {
        ringbuf_entry!(Trace::TofinoSequencerAbort(
            abort.state,
            abort.step,
            abort.error
        ));

        let power_rails =
            PowerRail::from_raw(self.sequencer.raw_power_rails()?)
                .map_err(SeqError::from)?;

        for rail in &power_rails {
            match rail.state {
                PowerRailState::GoodTimeout => {
                    ringbuf_entry!(Trace::TofinoPowerRailGoodTimeout(rail.id));
                }
                PowerRailState::Aborted => {
                    self.report_power_rail_abort(rail)?;
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn report_power_rail_abort(
        &mut self,
        rail: &PowerRail,
    ) -> Result<(), SeqError> {
        ringbuf_entry!(Trace::TofinoPowerRailAbort(rail.id, rail.pin_state));

        if rail.pin_state.fault {
            // TODO (arjen): pull PMBus for additional data in the case of
            // faults.
        }

        Ok(())
    }

    pub fn handle_tick(&mut self) -> Result<(), SeqError> {
        let status = self.sequencer.status()?;
        let error = status
            .abort
            .map_or(TofinoSeqError::None, |abort| abort.error);

        match &status.abort {
            Some(abort) if !self.abort_reported => {
                self.abort_reported = true;
                self.report_abort(abort)?;
            }
            Some(_) | None => {
                ringbuf_entry!(Trace::TofinoSequencerTick(
                    self.policy,
                    status.state,
                    error
                ));
            }
        }

        match (self.policy, status.state, error) {
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
