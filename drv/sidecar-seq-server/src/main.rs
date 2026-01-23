// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Sidecar sequencing process.

#![no_std]
#![no_main]

use crate::clock_generator::ClockGenerator;
use crate::front_io::FrontIOBoard;
use crate::tofino::Tofino;
use core::convert::Infallible;
use drv_fpga_api::{DeviceState, FpgaError, WriteOp};
use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_sidecar_front_io::phy_smi::PhyOscState;
use drv_sidecar_mainboard_controller::fan_modules::*;
use drv_sidecar_mainboard_controller::front_io::*;
use drv_sidecar_mainboard_controller::tofino2::*;
use drv_sidecar_mainboard_controller::MainboardController;
use drv_sidecar_seq_api::{
    FanModuleIndex, FanModulePresence, SeqError, TofinoSequencerPolicy,
};
use drv_stm32xx_sys_api as sys_api;
use fixedstr::FixedString;
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
task_slot!(SYS, sys);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

mod clock_generator;
mod front_io;
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
    TofinoVidAttempt(u8),
    TofinoVidAck,
    TofinoNoVid,
    TofinoNotInA0,
    TofinoInA0,
    TofinoEepromIdCode(u32),
    TofinoBar0RegisterValue(TofinoBar0Registers, u32),
    TofinoCfgRegisterValue(TofinoCfgRegisters, u32),
    TofinoPowerUp,
    TofinoPowerDown,
    SetVddCoreVout(userlib::units::Volts),
    SetPCIePresent,
    ClearPCIePresent,
    ClearingTofinoSequencerFault(TofinoSeqError),
    FrontIOBoardPowerEnable(bool),
    FrontIOBoardPowerFault,
    FrontIOBoardPowerNotGood,
    FrontIOBoardPowerGood,
    FrontIOBoardPresent,
    FrontIOBoardNotPresent,
    FrontIOBoardPhyPowerEnable(bool),
    FrontIOBoardPhyOscGood,
    FrontIOBoardPhyOscBad,
    LoadingFrontIOControllerBitstream {
        fpga_id: usize,
    },
    SkipLoadingFrontIOControllerBitstream {
        fpga_id: usize,
    },
    FrontIOControllerIdent {
        fpga_id: usize,
        ident: u32,
    },
    FrontIOControllerChecksum {
        fpga_id: usize,
        checksum: [u8; 4],
        expected: [u8; 4],
    },
    FpgaFanModuleFailure(FpgaError),
    FanModulePowerFault(FanModuleIndex, FanModuleStatus),
    FanModuleLedUpdate(FanModuleIndex, FanModuleLedState),
    FanModuleEnableUpdate(FanModuleIndex, FanModulePowerState),
    EreportSent(usize),
    EreportLost(usize, task_packrat_api::EreportWriteError),
    EreportTooBig,
}
ringbuf!(Trace, 32, Trace::None);

const TIMER_INTERVAL: u64 = 1000;

// QSFP_2_SP_A2_PG
const POWER_GOOD: sys_api::PinSet = sys_api::Port::F.pin(12);

const EREPORT_BUF_LEN: usize = microcbor::max_cbor_len_for!(
    task_packrat_api::Ereport<EreportClass, EreportKind>
);

#[derive(microcbor::Encode)]
pub enum EreportClass {
    #[cbor(rename = "hw.pwr.bmr491.mitfail")]
    Bmr491MitigationFailure,
}

#[derive(microcbor::EncodeFields)]
pub(crate) enum EreportKind {
    Bmr491MitigationFailure {
        refdes: FixedString<{ crate::i2c_config::MAX_COMPONENT_ID_LEN }>,
        failures: u32,
        last_cause: drv_i2c_devices::bmr491::MitigationFailureKind,
        succeeded: bool,
    },
}

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
    front_io_hsc: HotSwapController,
    front_io_board: Option<FrontIOBoard>,
    fan_modules: FanModules,
    // a piece of state to allow blinking LEDs to be in phase
    led_blink_on: bool,
    sys: sys_api::Sys,
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

    fn front_io_board_preinit(&self) -> Result<bool, SeqError> {
        // Make sure the front IO hot swap controller is enabled and good. The
        // power rail FSM will reach either the GoodTimeout, Aborted or Enabled
        // state or experience an FpgaError, so an open loop is safe.
        while match self.front_io_hsc.status()? {
            PowerRailStatus::GoodTimeout | PowerRailStatus::Aborted => {
                return Err(SeqError::FrontIOBoardPowerFault)
            }
            PowerRailStatus::Disabled => {
                self.front_io_hsc.set_enable(true)?;
                ringbuf_entry!(Trace::FrontIOBoardPowerEnable(true));

                true // Retry HSC status.
            }
            PowerRailStatus::RampingUp => {
                true // Retry HSC status.
            }
            PowerRailStatus::Enabled => false,
        } {
            userlib::hl::sleep_for(25);
        }

        // Check if the power is good via the PG pin
        if self.sys.gpio_read(POWER_GOOD) == 0 {
            ringbuf_entry!(Trace::FrontIOBoardPowerNotGood);
            return Err(SeqError::FrontIOPowerNotGood);
        } else {
            ringbuf_entry!(Trace::FrontIOBoardPowerGood);
        }

        // Determine if a front IO board is present.
        Ok(FrontIOBoard::present(I2C.get_task_id()))
    }

    fn front_io_phy_osc_good(&self) -> Result<bool, SeqError> {
        if let Some(front_io_board) = self.front_io_board.as_ref() {
            Ok(front_io_board.initialized()
                && front_io_board
                    .phy()
                    .osc_state()
                    .unwrap_or(PhyOscState::Unknown)
                    == PhyOscState::Good)
        } else {
            Err(SeqError::NoFrontIOBoard)
        }
    }

    fn actually_reset_front_io_phy(&mut self) -> Result<(), SeqError> {
        if let Some(front_io_board) = self.front_io_board.as_mut() {
            if front_io_board.initialized() {
                // The board was initialized prior and this function is called
                // by the monorail task because it is initializing the front IO
                // PHY. Unfortunately some front IO boards have PHY oscillators
                // which do not start reliably when their enable pin is used and
                // the only way to resolve this is by power cycling the front IO
                // board. But power cycling the board also bounces any QSFP
                // transceivers which may be running, so this function attempts
                // to determine what the monorail task wants to do.
                //
                // Whether or not the PHY oscillator was found to be operating
                // nominally is recorded in the front IO board controller. Look
                // up what this value is to determine if a power reset of the
                // front IO board is needed.
                match front_io_board.phy().osc_state()? {
                    PhyOscState::Bad => {
                        // The PHY was attempted to be initialized but its
                        // oscillator was deemed not functional. Unfortunately
                        // the only course of action is to power cycle the
                        // entire front IO board, so do so now.
                        self.front_io_hsc.set_enable(false)?;
                        ringbuf_entry!(Trace::FrontIOBoardPowerEnable(false));

                        // Wait some cool down period to allow caps to bleed off
                        // etc.
                        userlib::hl::sleep_for(1000);
                    }
                    PhyOscState::Good => {
                        // The PHY was initialized properly before and its
                        // oscillator declared operating nominally. Assume this
                        // has not changed and only a reset the PHY itself is
                        // desired.
                        front_io_board.phy().set_phy_power_enabled(false)?;
                        ringbuf_entry!(Trace::FrontIOBoardPhyPowerEnable(
                            false
                        ));

                        userlib::hl::sleep_for(10);
                    }
                    PhyOscState::Unknown => {
                        // Do nothing (yet) since the oscillator state is
                        // unknown.
                    }
                }
            }
        }

        // Run preinit to check HSC status.
        self.front_io_board_preinit()?;

        if let Some(front_io_board) = self.front_io_board.as_mut() {
            // At this point the front IO board has either not yet been
            // initalized or may have been power cycled and should be
            // initialized.
            if !front_io_board.initialized() {
                front_io_board.init()?;
            }

            // The PHY is still powered down. Request the sequencer to power up
            // and wait for it to be ready.
            front_io_board.phy().set_phy_power_enabled(true)?;
            ringbuf_entry!(Trace::FrontIOBoardPhyPowerEnable(true));

            while !front_io_board.phy().powered_up_and_ready()? {
                userlib::hl::sleep_for(20);
            }

            Ok(())
        } else {
            Err(SeqError::NoFrontIOBoard)
        }
    }

    // Determine if Tofino can be powered up. Some front IO boards were
    // assembled using oscillators which require the board to be power cycled
    // before they operate nominally. To avoid doing this and interrupting QSFP
    // interface initialization the chassis either should not have a front IO
    // board or the oscillator should have been found operating nominally before
    // transitioning to A0.
    fn ready_for_tofino_power_up(&self) -> Result<bool, SeqError> {
        match self.front_io_phy_osc_good() {
            Ok(osc_good) => Ok(osc_good),
            Err(SeqError::NoFrontIOBoard) => Ok(true),
            Err(e) => Err(e),
        }
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

    fn front_io_board_present(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.front_io_board.is_some())
    }

    fn front_io_board_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<SeqError>> {
        self.front_io_phy_osc_good().map_err(RequestError::from)
    }

    fn reset_front_io_phy(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<SeqError>> {
        self.actually_reset_front_io_phy()
            .map_err(RequestError::from)
    }

    fn set_front_io_phy_osc_state(
        &mut self,
        _: &RecvMessage,
        good: bool,
    ) -> Result<(), RequestError<SeqError>> {
        let front_io_board = self
            .front_io_board
            .as_ref()
            .ok_or(SeqError::NoFrontIOBoard)?;

        match front_io_board
            .phy()
            .osc_state()
            .map_err(SeqError::from)
            .map_err(RequestError::from)?
        {
            // The state of the oscillator has not yet been examined or was
            // marked bad in the previous run. Update as appropriate.
            PhyOscState::Unknown | PhyOscState::Bad => {
                ringbuf_entry!(if good {
                    Trace::FrontIOBoardPhyOscGood
                } else {
                    Trace::FrontIOBoardPhyOscBad
                });

                front_io_board
                    .phy()
                    .set_osc_good(good)
                    .map_err(SeqError::from)
                    .map_err(RequestError::from)
            }
            // The oscillator is already marked good and this state only changes
            // if it (and by extension the whole front IO board) is power
            // cycled. In that case the value of this register in the FPGA is
            // automatically reset when the bitstream is loaded and the other
            // arm of this match would be taken.
            //
            // So ignore this call if the oscillator has been found good since the last power
            // cycle of the front IO board.
            PhyOscState::Good => Ok(()),
        }
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
    ) -> Result<u32, RequestError<FpgaError>> {
        Ok(self.tofino.debug_port.read_direct(segment, offset)?)
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

    fn handle_notification(&mut self, bits: userlib::NotificationBits) {
        if !bits.has_timer_fired(notifications::TIMER_MASK) {
            return;
        }

        let start = sys_get_timer().now;

        // Determine if the front IO board has been initialized and no further
        // power interruptions are expected which would disrupt the main data
        // plane. See the comment of `ready_for_tofino_power_up` for more
        // context.
        if !self.tofino.ready_for_power_up {
            self.tofino.ready_for_power_up =
                self.ready_for_tofino_power_up().unwrap_or(false);
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
    let front_io_hsc = HotSwapController::new(MAINBOARD.get_task_id());
    let fan_modules = FanModules::new(MAINBOARD.get_task_id());
    let packrat = Packrat::from(PACKRAT.get_task_id());

    let ereport_buf = {
        use static_cell::ClaimOnceCell;
        static EREPORT_BUF: ClaimOnceCell<[u8; EREPORT_BUF_LEN]> =
            ClaimOnceCell::new([0; EREPORT_BUF_LEN]);
        EREPORT_BUF.claim()
    };

    // Apply the configuration mitigation on the BMR491, if required. This is an
    // external device access and may fail. We'll attempt it thrice and then
    // allow boot to continue.
    {
        use drv_i2c_devices::bmr491::{Bmr491, ExternalInputVoltageProtection};

        let dev = i2c_config::devices::bmr491_u12(I2C.get_task_id());
        let driver = Bmr491::new(&dev, 0);

        // Sidecar provides external undervoltage protection that is better than
        // what we'd get from the 491, so we rely on that.
        let protection = ExternalInputVoltageProtection::CutoffAt40V;

        let (failures, last_cause, succeeded) =
            match driver.apply_mitigation_for_rma2402311(protection) {
                Ok(r) => (r.failures, r.last_failure, true),
                Err(e) => (e.retries, Some(e.last_cause), false),
            };

        if let Some(last_cause) = last_cause {
            // Report the failure even if we eventually succeeded.
            try_send_ereport(
                &packrat,
                &mut ereport_buf[..],
                EreportClass::Bmr491MitigationFailure,
                EreportKind::Bmr491MitigationFailure {
                    refdes: FixedString::from_str(dev.component_id()),
                    failures,
                    last_cause,
                    succeeded,
                },
            );
        }
    }

    let sys = sys_api::Sys::from(SYS.get_task_id());
    sys.gpio_configure_input(POWER_GOOD, sys_api::Pull::None);

    let mut server = ServerImpl {
        mainboard_controller,
        clock_generator,
        tofino,
        front_io_hsc,
        front_io_board: None,
        fan_modules,
        led_blink_on: false,
        sys,
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
    match server.front_io_board_preinit() {
        Ok(true) => {
            ringbuf_entry!(Trace::FrontIOBoardPresent);

            let mut front_io_board = FrontIOBoard::new(
                FRONT_IO.get_task_id(),
                AUXFLASH.get_task_id(),
            );

            front_io_board.init().unwrap_lite();

            // TODO (arjen): check/load VPD data into packrat.

            // So far the front IO board looks functional. Assign it to the
            // server, implicitly marking it present for the lifetime of this
            // task.
            server.front_io_board = Some(front_io_board);
        }
        Ok(false) => ringbuf_entry!(Trace::FrontIOBoardNotPresent),
        Err(SeqError::FrontIOBoardPowerFault) => {
            ringbuf_entry!(Trace::FrontIOBoardPowerFault)
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

fn try_send_ereport(
    packrat: &task_packrat_api::Packrat,
    ereport_buf: &mut [u8],
    class: EreportClass,
    report: EreportKind,
) {
    let eresult = packrat.encode_ereport(
        &task_packrat_api::Ereport {
            class,
            version: 0,
            report,
        },
        ereport_buf,
    );
    match eresult {
        Ok(len) => ringbuf_entry!(Trace::EreportSent(len)),
        Err(task_packrat_api::EreportEncodeError::Packrat { len, err }) => {
            ringbuf_entry!(Trace::EreportLost(len, err))
        }
        Err(task_packrat_api::EreportEncodeError::Encoder(_)) => {
            ringbuf_entry!(Trace::EreportTooBig)
        }
    }
}

mod idl {
    use super::{
        DebugPortState, DirectBarSegment, FanModuleIndex, FanModulePresence,
        FanModuleStatus, FpgaError, SeqError, TofinoPcieReset, TofinoSeqError,
        TofinoSeqState, TofinoSeqStep, TofinoSequencerPolicy,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
