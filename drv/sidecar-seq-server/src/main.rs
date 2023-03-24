// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Sidecar sequencing process.

#![no_std]
#![no_main]

use crate::clock_generator::ClockGenerator;
use crate::front_io::FrontIOBoard;
use crate::tofino::Tofino;
use drv_fpga_api::{DeviceState, FpgaError, WriteOp};
use drv_i2c_api::{I2cDevice, ResponseCode};
use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_sidecar_mainboard_controller::tofino2::*;
use drv_sidecar_mainboard_controller::MainboardController;
use drv_sidecar_seq_api::{SeqError, TofinoSequencerPolicy};
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
    TofinoSequencerTick(TofinoSequencerPolicy, TofinoSeqState, TofinoSeqError),
    TofinoSequencerAbort(TofinoSeqState, TofinoSeqStep, TofinoSeqError),
    TofinoPowerRailGoodTimeout(PowerRails),
    TofinoPowerRailAbort(PowerRails, PowerRailPinState),
    TofinoVidAck,
    TofinoEepromIdCode(u32),
    TofinoBar0RegisterValue(TofinoBar0Registers, u32),
    TofinoCfgRegisterValue(TofinoCfgRegisters, u32),
    InitiateTofinoPowerUp,
    InitiateTofinoPowerDown,
    SetVddCoreVout(userlib::units::Volts),
    SetPCIePresent,
    ClearPCIePresent,
    ClearingTofinoSequencerFault(TofinoSeqError),
    FrontIOBoardPresent,
    NoFrontIOBoardPresent,
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
    FrontIOVsc8562Ready,
}
ringbuf!(Trace, 32, Trace::None);

const TIMER_INTERVAL: u64 = 1000;

struct ServerImpl {
    mainboard_controller: MainboardController,
    clock_generator: ClockGenerator,
    tofino: Tofino,
    front_io_board: FrontIOBoard,
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
    ) -> Result<[u8; 6], RequestError<SeqError>> {
        self.tofino
            .sequencer
            .raw_power_rails()
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

    fn front_io_board_present(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<SeqError>> {
        Ok(self.front_io_board.present())
    }

    fn front_io_phy_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<SeqError>> {
        if !self.front_io_board.present() {
            Err(SeqError::NoFrontIOBoard.into())
        } else {
            let phy_smi = self.front_io_board.phy_smi();
            Ok(phy_smi.phy_powered_up_and_ready().map_err(SeqError::from)?)
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
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
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

        sys_set_timer(Some(next_deadline), notifications::TIMER_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let mainboard_controller =
        MainboardController::new(MAINBOARD.get_task_id());
    let clock_generator = ClockGenerator::new(I2C.get_task_id());
    let tofino = Tofino::new(I2C.get_task_id());
    let front_io_board = FrontIOBoard::new(
        FRONT_IO.get_task_id(),
        I2C.get_task_id(),
        AUXFLASH.get_task_id(),
    );

    let mut server = ServerImpl {
        mainboard_controller,
        clock_generator,
        tofino,
        front_io_board,
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
                    let code = u32::try_from(e).unwrap();
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
    read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

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

    // Initialize a connected Front IO board.
    if server.front_io_board.present() {
        ringbuf_entry!(Trace::FrontIOBoardPresent);

        if !server.front_io_board.init().unwrap() {
            panic!();
        }

        let phy_smi = server.front_io_board.phy_smi();
        phy_smi.set_phy_power_enabled(true).unwrap();

        while !phy_smi.phy_powered_up_and_ready().unwrap() {
            userlib::hl::sleep_for(10);
        }

        ringbuf_entry!(Trace::FrontIOVsc8562Ready);
    } else {
        ringbuf_entry!(Trace::NoFrontIOBoardPresent);
    }

    // Configure the TMP451 attached to the Tofino to trigger its THERM_B
    // line at 90°C, rather than the default of 108°C.  The THERM_B line
    // is monitored by the sequencer FPGA and will cut power to the system,
    // because the Tofino doesn't have built-in protection against thermal
    // overruns.
    let i2c_task = I2C.get_task_id();
    let tmp451 = drv_i2c_devices::tmp451::Tmp451::new(
        &i2c_config::devices::tmp451_tf2(i2c_task),
        drv_i2c_devices::tmp451::Target::Remote,
    );
    tmp451
        .write_reg(drv_i2c_devices::tmp451::Register::RemoteTempThermBLimit, 90)
        .unwrap();

    // Before starting Tofino, we may need to clear sequencer abort state. This
    // will discard fault state when the SP resets, but this is acceptable for
    // now and an incentive to do more automated reporting.
    match &server.tofino.sequencer.status().unwrap().abort {
        Some(abort) => {
            server.tofino.report_abort(abort).unwrap();
            server.tofino.sequencer.clear_error().unwrap();
        }
        None => {}
    }

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
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{
        DebugPortState, DirectBarSegment, SeqError, TofinoPcieReset,
        TofinoSeqError, TofinoSeqState, TofinoSeqStep, TofinoSequencerPolicy,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
