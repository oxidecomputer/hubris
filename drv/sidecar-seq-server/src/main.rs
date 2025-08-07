// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Sidecar sequencing process.

#![no_std]
#![no_main]

use crate::clock_generator::ClockGenerator;
use crate::tofino::Tofino;
use core::convert::Infallible;
use drv_fpga_api::{DeviceState, FpgaError, WriteOp};
use drv_fpga_user_api::power_rail::{PowerRailStatus, RawPowerRailState};
use drv_front_io_api::{FrontIO, FrontIOError, FrontIOStatus};
use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_sidecar_mainboard_controller::fan_modules::*;
use drv_sidecar_mainboard_controller::tofino2::*;
use drv_sidecar_mainboard_controller::MainboardController;
use drv_sidecar_seq_api::{
    FanModuleIndex, FanModulePresence, SeqError, TofinoSequencerPolicy,
};
use idol_runtime::{
    ClientError, Leased, NotificationHandler, RequestError, R, W,
};
use ringbuf::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(MAINBOARD, mainboard);
task_slot!(FRONT_IO, front_io);
task_slot!(AUXFLASH, auxflash);
task_slot!(PACKRAT, packrat);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

mod clock_generator;
mod tofino;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    FpgaInit,
    FpgaInitComplete,
    FpgaBitstreamError(u32),
    LoadingFpgaBitstream,
    SkipLoadingBitstream,
    MainboardControllerId(u32),
    MainboardControllerChecksum(u32),
    MainboardControllerVersion(u32),
    MainboardControllerSha(u32),
    InvalidMainboardControllerId(u32),
    ExpectedMainboardControllerChecksum(u32),
    LoadingClockConfiguration,
    SkipLoadingClockConfiguration,
    ClockConfigurationError(usize, ResponseCode),
    ClockConfigurationComplete,
    TofinoSequencerError(SeqError),
    TofinoSequencerPolicyUpdate(TofinoSequencerPolicy),
    TofinoSequencerTick(TofinoSequencerPolicy, TofinoStateDetails),
    TofinoSequencerAbort {
        state: TofinoSeqState,
        step: TofinoSeqStep,
        error: TofinoSeqError,
    },
    TofinoPowerRail(TofinoPowerRailId, PowerRailStatus),
    TofinoVidAck,
    TofinoEepromIdCode(u32),
    TofinoBar0RegisterValue(TofinoBar0Registers, u32),
    TofinoCfgRegisterValue(TofinoCfgRegisters, u32),
    TofinoPowerUp,
    TofinoPowerDown,
    SetVddCoreVout(userlib::units::Volts),
    SetPCIePresent,
    ClearPCIePresent,
    ClearingTofinoSequencerFault(TofinoSeqError),
    FrontIOBoardPowerProblem,
    FrontIOBoardPresent,
    FrontIOBoardNotPresent,
    FpgaFanModuleFailure(FpgaError),
    FanModulePowerFault(FanModuleIndex, FanModuleStatus),
    FanModuleLedUpdate(FanModuleIndex, FanModuleLedState),
    FanModuleEnableUpdate(FanModuleIndex, FanModulePowerState),
}
ringbuf!(Trace, 32, Trace::None);

const TIMER_INTERVAL: u64 = 1000;

#[derive(Copy, Clone, PartialEq)]
enum TofinoStateDetails {
    A0 {
        pcie_link: bool,
    },
    A2 {
        error: TofinoSeqError,
    },
    Other {
        state: TofinoSeqState,
        step: TofinoSeqStep,
        error: TofinoSeqError,
    },
}

struct ServerImpl {
    mainboard_controller: MainboardController,
    clock_generator: ClockGenerator,
    tofino: Tofino,
    front_io_board: FrontIO,
    fan_modules: FanModules,
    // a piece of state to allow blinking LEDs to be in phase
    led_blink_on: bool,
}

impl ServerImpl {
    fn set_fan_module_led_state(
        &mut self,
        module: FanModuleIndex,
        state: FanModuleLedState,
    ) {
        ringbuf_entry!(Trace::FanModuleLedUpdate(module, state));
        self.fan_modules.set_led_state(module, state);
    }

    fn set_fan_module_power_state(
        &mut self,
        module: FanModuleIndex,
        state: FanModulePowerState,
    ) {
        ringbuf_entry!(Trace::FanModuleEnableUpdate(module, state));
        self.fan_modules.set_power_state(module, state);
    }

    // The SP does not need to disable the module when presence is lost because
    // the FPGA does that automatically. The SP does need to re-enable it when
    // presence is detected again.
    fn monitor_fan_modules(&mut self) {
        match self.fan_modules.get_status() {
            Ok(status) => {
                for (module, status) in status.iter().enumerate() {
                    let module =
                        FanModuleIndex::from_usize(module).unwrap_lite();
                    // Fan module is not present, make sure the LED isn't driven
                    // Avoid setting the state to Off if is already off so the
                    // ringbuf is not spammed.
                    if !status.present() {
                        if self.fan_modules.get_led_state(module)
                            != FanModuleLedState::Off
                        {
                            self.set_fan_module_led_state(
                                module,
                                FanModuleLedState::Off,
                            );
                        }

                    // Fan module is present but disabled and should be enabled
                    } else if !status.enable() {
                        self.set_fan_module_led_state(
                            module,
                            FanModuleLedState::On,
                        );

                    // Power fault has been observed for the module, disable it
                    } else if status.power_fault() || status.power_timed_out() {
                        ringbuf_entry!(Trace::FanModulePowerFault(
                            module, *status
                        ));
                        self.set_fan_module_power_state(
                            module,
                            FanModulePowerState::Disabled,
                        )
                    }
                }
            }
            Err(e) => ringbuf_entry!(Trace::FpgaFanModuleFailure(e)),
        }
        if let Err(e) = self.fan_modules.update_power() {
            ringbuf_entry!(Trace::FpgaFanModuleFailure(e));
        }
        if let Err(e) = self.fan_modules.update_leds(self.led_blink_on) {
            ringbuf_entry!(Trace::FpgaFanModuleFailure(e));
        }
    }

    // Determine if Tofino can be powered up. Some front IO boards were
    // assembled using oscillators which require the board to be power cycled
    // before they operate nominally. To avoid doing this and interrupting QSFP
    // interface initialization the chassis either should not have a front IO
    // board or the oscillator should have been found operating nominally before
    // transitioning to A0.
    fn ready_for_tofino_power_up(&self) -> bool {
        matches!(
            self.front_io_board.board_status(),
            FrontIOStatus::NotPresent | FrontIOStatus::Ready
        )
    }
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

    fn tofino_seq_error_step(
        &mut self,
        _: &RecvMessage,
    ) -> Result<TofinoSeqStep, RequestError<SeqError>> {
        self.tofino
            .sequencer
            .error_step()
            .map_err(SeqError::from)
            .map_err(RequestError::from)
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

    fn tofino_power_rails(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[RawPowerRailState; 6], RequestError<SeqError>> {
        self.tofino
            .sequencer
            .power_rail_states()
            .map_err(SeqError::from)
            .map_err(RequestError::from)
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

    fn tofino_pcie_link_up(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.tofino.pcie_link_up)
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
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.clock_generator.config_loaded)
    }

    fn tofino_debug_port_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<DebugPortState, RequestError<SeqError>> {
        Ok(self.tofino.debug_port.state().map_err(SeqError::from)?)
    }

    fn set_tofino_debug_port_state(
        &mut self,
        _: &RecvMessage,
        state: DebugPortState,
    ) -> Result<(), RequestError<SeqError>> {
        Ok(self
            .tofino
            .debug_port
            .set_state(state)
            .map_err(SeqError::from)?)
    }

    fn tofino_read_direct(
        &mut self,
        _: &RecvMessage,
        segment: DirectBarSegment,
        offset: u32,
    ) -> Result<u32, RequestError<SeqError>> {
        Ok(self
            .tofino
            .debug_port
            .read_direct(segment, offset)
            .map_err(SeqError::from)?)
    }

    fn tofino_write_direct(
        &mut self,
        _: &RecvMessage,
        segment: DirectBarSegment,
        offset: u32,
        value: u32,
    ) -> Result<(), RequestError<SeqError>> {
        Ok(self
            .tofino
            .debug_port
            .write_direct(segment, offset, value)
            .map_err(SeqError::from)?)
    }

    fn spi_eeprom_idcode(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<SeqError>> {
        Ok(self
            .tofino
            .debug_port
            .spi_eeprom_idcode()
            .map_err(SeqError::from)?)
    }

    fn spi_eeprom_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<SeqError>> {
        Ok(self
            .tofino
            .debug_port
            .spi_eeprom_status()
            .map_err(SeqError::from)?)
    }

    fn read_spi_eeprom_bytes(
        &mut self,
        _: &RecvMessage,
        offset: u32,
        data: Leased<W, [u8]>,
    ) -> Result<(), RequestError<SeqError>> {
        let mut buf = [0u8; 128];
        let mut eeprom_offset = offset as usize;
        let mut data_offset = 0;
        let eeprom_end = offset as usize + data.len();

        while eeprom_offset < eeprom_end {
            let amount = (eeprom_end - eeprom_offset).min(buf.len());
            self.tofino
                .debug_port
                .read_spi_eeprom_bytes(eeprom_offset, &mut buf[..amount])
                .map_err(SeqError::from)?;
            data.write_range(
                data_offset..(data_offset + amount),
                &buf[..amount],
            )
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            data_offset += amount;
            eeprom_offset += amount;
        }

        Ok(())
    }

    fn write_spi_eeprom_bytes(
        &mut self,
        _: &RecvMessage,
        offset: u32,
        data: Leased<R, [u8]>,
    ) -> Result<(), RequestError<SeqError>> {
        let mut buf = [0u8; 128];
        let mut eeprom_offset = offset as usize;
        let mut data_offset = 0;
        let eeprom_end = offset as usize + data.len();

        while eeprom_offset < eeprom_end {
            let amount = (eeprom_end - eeprom_offset).min(buf.len());
            data.read_range(
                data_offset..(data_offset + amount),
                &mut buf[..amount],
            )
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            self.tofino
                .debug_port
                .write_spi_eeprom_bytes(eeprom_offset, &buf[..amount])
                .map_err(SeqError::from)?;
            data_offset += amount;
            eeprom_offset += amount;
        }

        Ok(())
    }

    fn mainboard_controller_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<SeqError>> {
        self.mainboard_controller
            .ready()
            .map_err(SeqError::from)
            .map_err(RequestError::from)
    }

    fn fan_module_status(
        &mut self,
        _: &RecvMessage,
        module: FanModuleIndex,
    ) -> Result<FanModuleStatus, RequestError<SeqError>> {
        match self.fan_modules.get_status() {
            Ok(all_modules) => Ok(all_modules[module as usize]),
            Err(e) => Err(RequestError::from(SeqError::from(e))),
        }
    }

    fn fan_module_presence(
        &mut self,
        _: &RecvMessage,
    ) -> Result<FanModulePresence, RequestError<SeqError>> {
        Ok(FanModulePresence(self.fan_modules.get_presence()))
    }

    fn fan_module_led_off(
        &mut self,
        _: &RecvMessage,
        module: FanModuleIndex,
    ) -> Result<(), RequestError<SeqError>> {
        self.set_fan_module_led_state(module, FanModuleLedState::Off);
        Ok(())
    }

    fn fan_module_led_on(
        &mut self,
        _: &RecvMessage,
        module: FanModuleIndex,
    ) -> Result<(), RequestError<SeqError>> {
        self.set_fan_module_led_state(module, FanModuleLedState::On);
        Ok(())
    }

    fn fan_module_led_blink(
        &mut self,
        _: &RecvMessage,
        module: FanModuleIndex,
    ) -> Result<(), RequestError<SeqError>> {
        self.set_fan_module_led_state(module, FanModuleLedState::Blink);
        Ok(())
    }

    fn fan_module_enable(
        &mut self,
        _: &RecvMessage,
        module: FanModuleIndex,
    ) -> Result<(), RequestError<SeqError>> {
        self.set_fan_module_power_state(module, FanModulePowerState::Enabled);
        Ok(())
    }

    fn fan_module_disable(
        &mut self,
        _: &RecvMessage,
        module: FanModuleIndex,
    ) -> Result<(), RequestError<SeqError>> {
        self.set_fan_module_power_state(module, FanModulePowerState::Disabled);
        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        let start = sys_get_timer().now;

        // Determine if the front IO board has been initialized and no further
        // power interruptions are expected which would disrupt the main data
        // plane. See the comment of `ready_for_tofino_power_up` for more
        // context.
        if !self.tofino.ready_for_power_up {
            self.tofino.ready_for_power_up = self.ready_for_tofino_power_up();
        }

        if let Err(e) = self.tofino.handle_tick() {
            ringbuf_entry!(Trace::TofinoSequencerError(e));
        }

        // Change status of LED blink variable, keeping anything gating on/off
        // with it in phase
        self.led_blink_on = !self.led_blink_on;

        // Fan module monitoring pulled out to keep this loop readable
        self.monitor_fan_modules();

        let finish = sys_get_timer().now;

        // We now know when we were notified and when any work was completed.
        // Note that the assumption here is that `start` < `finish` and that
        // this won't hold if the system time rolls over. But, the system timer
        // is a u64, with each bit representing a ms, so in practice this should
        // be fine. Anyway, armed with this information, find the next deadline
        // some multiple of `TIMER_INTERVAL` in the future.

        // The timer is monotonic, so finish >= start, so we use wrapping_add
        // here to avoid an overflow check that the compiler conservatively
        // inserts.
        let delta = finish.wrapping_sub(start);
        let next_deadline = finish + TIMER_INTERVAL - (delta % TIMER_INTERVAL);

        sys_set_timer(Some(next_deadline), notifications::TIMER_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let i2c_task = I2C.get_task_id();
    let mainboard_controller =
        MainboardController::new(MAINBOARD.get_task_id());
    let clock_generator = ClockGenerator::new(i2c_task);
    let tofino = Tofino::new(i2c_task);
    let fan_modules = FanModules::new(MAINBOARD.get_task_id());
    let front_io_board = FrontIO::from(FRONT_IO.get_task_id());

    let mut server = ServerImpl {
        mainboard_controller,
        clock_generator,
        tofino,
        front_io_board,
        fan_modules,
        led_blink_on: false,
    };

    ringbuf_entry!(Trace::FpgaInit);

    match server
        .mainboard_controller
        .await_fpga_ready(25)
        .unwrap_or(DeviceState::Unknown)
    {
        DeviceState::AwaitingBitstream => {
            ringbuf_entry!(Trace::LoadingFpgaBitstream);

            match server
                .mainboard_controller
                .load_bitstream(AUXFLASH.get_task_id())
            {
                Err(e) => {
                    let code = u32::from(e);
                    ringbuf_entry!(Trace::FpgaBitstreamError(code));

                    // If this is an auxflash error indicating that we can't
                    // find the target blob, then it's possible that data isn't
                    // present (i.e. this is an initial boot at the factory). To
                    // prevent this task from spinning too hard, we add a brief
                    // delay before resetting.
                    //
                    // Note that other auxflash errors (e.g. a failed read) will
                    // reset immediately, matching existing behavior on a failed
                    // FPGA reset.
                    if matches!(e, FpgaError::AuxMissingBlob) {
                        userlib::hl::sleep_for(100);
                    }
                    panic!();
                }
                // Set the checksum write-once registers to lock the design
                // until the FPGA is reset.
                Ok(()) => {
                    server
                        .mainboard_controller
                        .set_short_bitstream_checksum()
                        .unwrap_lite();
                }
            }
        }
        DeviceState::RunningUserDesign => {
            ringbuf_entry!(Trace::SkipLoadingBitstream);
        }
        _ => panic!(),
    }

    // Read the design Ident and determine if a bitstream reload is needed.
    let ident = server.mainboard_controller.read_ident().unwrap_lite();

    match ident.id.into() {
        MainboardController::EXPECTED_ID => {
            ringbuf_entry!(Trace::MainboardControllerId(ident.id.into()))
        }
        _ => {
            // The FPGA is running something unexpected. Reset the device and
            // fire the escape thrusters. This will force a bitstream load when
            // the task is restarted.
            ringbuf_entry!(Trace::InvalidMainboardControllerId(
                ident.id.into()
            ));
            server.mainboard_controller.reset().unwrap_lite();
            panic!()
        }
    }

    ringbuf_entry!(Trace::MainboardControllerChecksum(ident.checksum.into()));

    if !server
        .mainboard_controller
        .short_bitstream_checksum_valid(&ident)
    {
        ringbuf_entry!(Trace::ExpectedMainboardControllerChecksum(
            MainboardController::short_bitstream_checksum()
        ));

        // The mainboard controller does not match the checksum of the
        // bitstream which is expected to run. This means the register map
        // may not match the APIs in this binary so a bitstream reload is
        // required.
        //
        // Attempt to shutdown Tofino somewhat gracefully instead of letting
        // the PDN collapse. This may need some tweaking because the host
        // CPU will suddenly lose its PCIe device. Perhaps an interrupt is
        // in order here to prep the driver for what's about to happen.
        if let Ok(()) = server.tofino.power_down() {
            // Give the sequencer some time to shut down the PDN.
            userlib::hl::sleep_for(25);
        }

        // Reset the FPGA and deploy the parashutes. This will cause the
        // bitstream to be reloaded when the task is restarted.
        server.mainboard_controller.reset().unwrap_lite();
        panic!()
    }

    // The expected version of the mainboard controller is running. Log some
    // more details.
    ringbuf_entry!(Trace::MainboardControllerVersion(ident.version.into()));
    ringbuf_entry!(Trace::MainboardControllerSha(ident.sha.into()));
    ringbuf_entry!(Trace::FpgaInitComplete);

    // Populate packrat with our mac address and identity.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, i2c_task);

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
        .unwrap_or(TofinoSeqState::Init)
    {
        ringbuf_entry!(Trace::SkipLoadingClockConfiguration);
        server.clock_generator.config_loaded = true;
        server.tofino.policy = TofinoSequencerPolicy::LatchOffOnFault;
    } else if server.clock_generator.load_config().is_err() {
        panic!()
    }
    ringbuf_entry!(Trace::ClockConfigurationComplete);

    // Enable the front IO hot swap controller and probe for a front IO board.
    match server.front_io_board.power_on() {
        Ok(_) => {
            if server.front_io_board.board_present() {
                ringbuf_entry!(Trace::FrontIOBoardPresent);
                // TODO: check/load VPD data into packrat.
            } else {
                ringbuf_entry!(Trace::FrontIOBoardNotPresent)
            }
        }
        Err(FrontIOError::PowerFault | FrontIOError::PowerNotGood) => {
            ringbuf_entry!(Trace::FrontIOBoardPowerProblem)
        }
        // Something went wrong getting the HSC status, eject.
        Err(_) => panic!("unknown front IO board preinit failure"),
    }

    // Configure the TMP451 attached to the Tofino to trigger its THERM_B
    // line at 90°C, rather than the default of 108°C.  The THERM_B line
    // is monitored by the sequencer FPGA and will cut power to the system,
    // because the Tofino doesn't have built-in protection against thermal
    // overruns.
    let tmp451 = drv_i2c_devices::tmp451::Tmp451::new(
        &i2c_config::devices::tmp451_tf2(i2c_task),
        drv_i2c_devices::tmp451::Target::Remote,
    );
    tmp451
        .write_reg(drv_i2c_devices::tmp451::Register::RemoteTempThermBLimit, 90)
        .unwrap_lite();

    // Before starting Tofino, we may need to clear sequencer abort state. This
    // will discard fault state when the SP resets, but this is acceptable for
    // now and an incentive to do more automated reporting.
    match &server.tofino.sequencer.status().unwrap_lite().abort {
        Some(abort) => {
            server.tofino.report_abort(abort).unwrap_lite();
            server.tofino.sequencer.clear_error().unwrap_lite();
        }
        None => {}
    }

    // Clear debug port state in the FPGA
    server.tofino.debug_port.reset().unwrap_lite();

    // Power on, unless suppressed by the `stay-in-a2` feature
    if !cfg!(feature = "stay-in-a2") {
        server.tofino.policy = TofinoSequencerPolicy::LatchOffOnFault;
    }

    //
    // This will put our timer in the past, and should immediately kick us.
    //
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{
        DebugPortState, DirectBarSegment, FanModuleIndex, FanModulePresence,
        FanModuleStatus, SeqError, TofinoPcieReset, TofinoSeqError,
        TofinoSeqState, TofinoSeqStep, TofinoSequencerPolicy,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
