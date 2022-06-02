// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Sidecar sequencing process.

#![no_std]
#![no_main]

use drv_fpga_api::{DeviceState, FpgaError, WriteOp};
use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_i2c_devices::raa229618::Raa229618;
use drv_sidecar_mainboard_controller_api::tofino2::{
    Sequencer as TofinoSequencer, Tofino2Vid, TofinoPcieReset, TofinoSeqError,
    TofinoSeqState,
};
use drv_sidecar_mainboard_controller_api::MainboardController;
use drv_sidecar_seq_api::{SeqError, TofinoSequencerPolicy};
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FPGA, fpga);

mod payload;

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
use i2c_config::devices;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    FpgaInit,
    FpgaBitstreamError(u32),
    LoadingFpgaBitstream,
    SkipLoadingBitstream,
    FpgaInitComplete,
    ValidMainboardControllerIdent(u32),
    InvalidMainboardControllerIdent(u32),
    LoadingClockConfiguration,
    SkipLoadingClockConfiguration,
    ClockConfigurationError(usize, ResponseCode),
    ClockConfigurationComplete,
    TofinoSequencerPolicyUpdate(TofinoSequencerPolicy),
    TofinoSequencerTick(TofinoSequencerPolicy, TofinoSeqState, TofinoSeqError),
    TofinoSequencerError(SeqError),
    TofinoSequencerFault(TofinoSeqError),
    TofinoVidAck,
    InitiateTofinoPowerUp,
    InitiateTofinoPowerDown,
    SetVddCoreVout(userlib::units::Volts),
    SetPCIePresent,
    ClearPCIePresent,
    ClearingTofinoSequencerFault(TofinoSeqError),
}

ringbuf!(Trace, 32, Trace::None);

const TIMER_NOTIFICATION_MASK: u32 = 1 << 0;
const TIMER_INTERVAL: u64 = 1000;

struct ClockGenerator {
    device: I2cDevice,
    config_loaded: bool,
}

impl ClockGenerator {
    fn load_config(&mut self) -> Result<(), SeqError> {
        ringbuf_entry!(Trace::LoadingClockConfiguration);

        let mut packet = 0;

        payload::idt8a3xxxx_payload(|buf| match self.device.write(buf) {
            Err(err) => {
                ringbuf_entry!(Trace::ClockConfigurationError(packet, err));
                Err(SeqError::ClockConfigurationFailed)
            }

            Ok(_) => {
                packet += 1;
                Ok(())
            }
        })?;

        self.config_loaded = true;
        Ok(())
    }
}

struct Tofino {
    policy: TofinoSequencerPolicy,
    sequencer: TofinoSequencer,
    vddcore: Raa229618,
}

impl Tofino {
    fn apply_vid(&mut self, vid: Tofino2Vid) -> Result<(), SeqError> {
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

    fn set_pcie_present(&mut self, present: bool) -> Result<(), SeqError> {
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

    fn power_up(&mut self) -> Result<(), SeqError> {
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

    fn power_down(&mut self) -> Result<(), SeqError> {
        ringbuf_entry!(Trace::InitiateTofinoPowerDown);
        self.set_pcie_present(false)?;
        self.sequencer.set_pcie_reset(TofinoPcieReset::Asserted)?;
        self.sequencer
            .set_enable(false)
            .map_err(|_| SeqError::SequencerError)
    }

    fn handle_tick(&mut self) -> Result<(), SeqError> {
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

struct ServerImpl {
    mainboard_controller: MainboardController,
    clock_generator: ClockGenerator,
    tofino: Tofino,
}

impl idl::InOrderSequencerImpl for ServerImpl {
    fn tofino_seq_policy(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<TofinoSequencerPolicy, RequestError<SeqError>> {
        Ok(self.tofino.policy)
    }

    fn set_tofino_seq_policy(
        &mut self,
        _msg: &userlib::RecvMessage,
        policy: TofinoSequencerPolicy,
    ) -> Result<(), RequestError<SeqError>> {
        ringbuf_entry!(Trace::TofinoSequencerPolicyUpdate(policy));
        self.tofino.policy = policy;
        Ok(())
    }

    fn tofino_seq_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<TofinoSeqState, RequestError<SeqError>> {
        Ok(self.tofino.sequencer.state().map_err(SeqError::from)?)
    }

    fn tofino_seq_error(
        &mut self,
        _: &RecvMessage,
    ) -> Result<TofinoSeqError, RequestError<SeqError>> {
        Ok(self.tofino.sequencer.error().map_err(SeqError::from)?)
    }

    fn clear_tofino_seq_error(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SeqError>> {
        if let Ok(e) = self.tofino.sequencer.error().map_err(SeqError::from) {
            ringbuf_entry!(Trace::ClearingTofinoSequencerFault(e));
        }
        Ok(self
            .tofino
            .sequencer
            .clear_error()
            .map_err(SeqError::from)?)
    }

    fn tofino_power_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<SeqError>> {
        Ok(self
            .tofino
            .sequencer
            .power_status()
            .map_err(SeqError::from)?)
    }

    fn tofino_pcie_hotplug_ctrl(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u8, RequestError<SeqError>> {
        Ok(self
            .tofino
            .sequencer
            .pcie_hotplug_ctrl()
            .map_err(SeqError::from)?)
    }

    fn set_tofino_pcie_hotplug_ctrl(
        &mut self,
        _: &userlib::RecvMessage,
        mask: u8,
    ) -> Result<(), RequestError<SeqError>> {
        Ok(self
            .tofino
            .sequencer
            .write_pcie_hotplug_ctrl(WriteOp::BitSet, mask)
            .map_err(SeqError::from)?)
    }

    fn clear_tofino_pcie_hotplug_ctrl(
        &mut self,
        _: &userlib::RecvMessage,
        mask: u8,
    ) -> Result<(), RequestError<SeqError>> {
        Ok(self
            .tofino
            .sequencer
            .write_pcie_hotplug_ctrl(WriteOp::BitClear, mask)
            .map_err(SeqError::from)?)
    }

    fn tofino_pcie_reset(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<TofinoPcieReset, RequestError<SeqError>> {
        Ok(self.tofino.sequencer.pcie_reset().map_err(SeqError::from)?)
    }

    fn set_tofino_pcie_reset(
        &mut self,
        _: &userlib::RecvMessage,
        reset: TofinoPcieReset,
    ) -> Result<(), RequestError<SeqError>> {
        Ok(self
            .tofino
            .sequencer
            .set_pcie_reset(reset)
            .map_err(SeqError::from)?)
    }

    fn tofino_pcie_hotplug_status(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u8, RequestError<SeqError>> {
        Ok(self
            .tofino
            .sequencer
            .pcie_hotplug_status()
            .map_err(SeqError::from)?)
    }

    fn load_clock_config(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SeqError>> {
        Ok(self.clock_generator.load_config()?)
    }

    fn is_clock_config_loaded(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<SeqError>> {
        Ok(self.clock_generator.config_loaded)
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        TIMER_NOTIFICATION_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        let start = sys_get_timer().now;

        if let Err(e) = self.tofino.handle_tick() {
            ringbuf_entry!(Trace::TofinoSequencerError(e));
        }

        let finish = sys_get_timer().now;

        // We now know when we were notified and when any work was completed.
        // Note that the assumption here is that `start` < `finish` and that
        // this won't hold if the system time rolls over. But, the system timer
        // is a u64, with each bit representing a ms, so in practice this should
        // be fine. Anyway, armed with this information, find the next deadline
        // some multiple of `TIMER_INTERVAL` in the future.

        let delta = finish - start;
        let next_deadline = finish + TIMER_INTERVAL - (delta % TIMER_INTERVAL);

        sys_set_timer(Some(next_deadline), TIMER_NOTIFICATION_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];
    let deadline = sys_get_timer().now;

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    sys_set_timer(Some(deadline), TIMER_NOTIFICATION_MASK);

    let mainboard_controller = MainboardController::new(FPGA.get_task_id());

    let clock_generator = ClockGenerator {
        device: devices::idt8a34001(I2C.get_task_id())[0],
        config_loaded: false,
    };

    let (i2c_device, rail) =
        i2c_config::pmbus::v0p8_tf2_vdd_core(I2C.get_task_id());
    let vddcore = Raa229618::new(&i2c_device, rail);
    let tofino = Tofino {
        policy: TofinoSequencerPolicy::Disabled,
        sequencer: TofinoSequencer::new(FPGA.get_task_id()),
        vddcore,
    };

    let mut server = ServerImpl {
        mainboard_controller,
        clock_generator,
        tofino,
    };

    ringbuf_entry!(Trace::FpgaInit);

    match server
        .mainboard_controller
        .await_fpga_ready(25)
        .unwrap_or(DeviceState::Unknown)
    {
        DeviceState::AwaitingBitstream => {
            ringbuf_entry!(Trace::LoadingFpgaBitstream);

            if let Err(e) = server.mainboard_controller.load_bitstream() {
                ringbuf_entry!(Trace::FpgaBitstreamError(
                    u32::try_from(e).unwrap()
                ));
                panic!();
            }
        }
        DeviceState::RunningUserDesign => {
            ringbuf_entry!(Trace::SkipLoadingBitstream);
        }
        _ => panic!(),
    }

    ringbuf_entry!(Trace::FpgaInitComplete);

    let ident = server.mainboard_controller.ident().unwrap();
    if !server.mainboard_controller.ident_valid(ident) {
        ringbuf_entry!(Trace::InvalidMainboardControllerIdent(ident));
        panic!();
    }
    ringbuf_entry!(Trace::ValidMainboardControllerIdent(ident));

    // The sequencer for the clock generator currently does not have a feedback
    // mechanism/register we can read. Sleeping a short while seems to be
    // sufficient for now.
    //
    // TODO (arjen): Implement reset control through the mainboard controller.
    userlib::hl::sleep_for(100);

    if let TofinoSeqState::A0 = server
        .tofino
        .sequencer
        .state()
        .unwrap_or(TofinoSeqState::Initial)
    {
        ringbuf_entry!(Trace::SkipLoadingClockConfiguration);
        server.clock_generator.config_loaded = true;
        server.tofino.policy = TofinoSequencerPolicy::LatchOffOnFault;
    } else if server.clock_generator.load_config().is_err() {
        panic!()
    }
    ringbuf_entry!(Trace::ClockConfigurationComplete);

    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{
        SeqError, TofinoPcieReset, TofinoSeqError, TofinoSeqState,
        TofinoSequencerPolicy,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
