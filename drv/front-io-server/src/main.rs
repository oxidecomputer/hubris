// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Front IO board

#![no_std]
#![no_main]

use core::convert::Infallible;
use drv_fpga_api::{DeviceState, FpgaError, WriteOp};
use drv_front_io_api::{
    controller::FrontIOController,
    leds::{FullErrorSummary, Leds},
    phy_smi::{PhyOscState, PhySmi},
    transceivers::{
        LedStates, LogicalPort, LogicalPortMask, ModuleResult,
        ModuleResultNoFailure, PortI2CStatus, TransceiverStatus, Transceivers,
        NUM_PORTS, PAGE_SIZE_BYTES,
    },
    Addr, FrontIOError, FrontIOStatus, LedState, Reg,
};
use drv_i2c_devices::{at24csw080::At24Csw080, pca9956b, Validate};
use enum_map::Enum;
use idol_runtime::{
    ClientError, Leased, NotificationHandler, RequestError, R, W,
};
use multitimer::{Multitimer, Repeat};
use ringbuf::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(AUXFLASH, auxflash);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    SequenceUpdate(FrontIOStatus),
    FpgaInitError(FpgaError),
    PhyOscGood,
    PhyOscBad,
    LEDInitComplete,
    LEDInitError(pca9956b::Error),
    LEDUpdateError(pca9956b::Error),
    LEDReadError(pca9956b::Error),
    LEDErrorSummary(FullErrorSummary),
    LEDUninitialized,
    FpgaBitstreamError(u32),
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
}
ringbuf!(Trace, 32, Trace::None);

struct ServerImpl {
    /// Handle for the auxflash task
    auxflash_task: userlib::TaskId,

    /// Handles for each FPGA
    controllers: [FrontIOController; 2],

    /// Interface for the LED controllers
    leds: Leds,

    /// VSC8562 SMI Interface
    phy_smi: PhySmi,

    /// Interface for the trnasceivers
    transceivers: Transceivers,

    /// Status of the Front IO board
    board_status: FrontIOStatus,

    /// State to allow blinking LEDs to be in phase
    led_blink_on: bool,
    /// State around LED management
    led_error: FullErrorSummary,
    leds_initialized: bool,
    led_states: LedStates,
    system_led_state: LedState,
}

/// Controls how often we update the LED controllers (in milliseconds).
const I2C_INTERVAL: u64 = 100;

/// Blink LEDs at a 50% duty cycle (in milliseconds)
const BLINK_INTERVAL: u64 = 500;

/// How often we should attempt the next sequencing step (in milliseconds)
const SEQ_INTERVAL: u64 = 500;

impl ServerImpl {
    fn initialized(&self) -> bool {
        self.controllers.iter().all(|c| c.ready().unwrap_or(false))
    }

    fn fpga_init(&mut self) -> Result<bool, FpgaError> {
        let mut controllers_ready = true;

        for (i, controller) in self.controllers.iter_mut().enumerate() {
            let state = controller.await_fpga_ready(25)?;
            let mut ident;
            let mut ident_valid = false;
            let mut checksum;
            let mut checksum_valid = false;

            if state == DeviceState::RunningUserDesign {
                (ident, ident_valid) = controller.ident_valid()?;
                ringbuf_entry!(Trace::FrontIOControllerIdent {
                    fpga_id: i,
                    ident
                });

                (checksum, checksum_valid) = controller.checksum_valid()?;
                ringbuf_entry!(Trace::FrontIOControllerChecksum {
                    fpga_id: i,
                    checksum,
                    expected: FrontIOController::short_checksum(),
                });

                if !ident_valid || !checksum_valid {
                    // Attempt to correct the invalid IDENT by reloading the
                    // bitstream.
                    controller.fpga_reset()?;
                }
            }

            if ident_valid && checksum_valid {
                ringbuf_entry!(Trace::SkipLoadingFrontIOControllerBitstream {
                    fpga_id: i
                });
            } else {
                ringbuf_entry!(Trace::LoadingFrontIOControllerBitstream {
                    fpga_id: i
                });

                if let Err(e) = controller.load_bitstream(self.auxflash_task) {
                    ringbuf_entry!(Trace::FpgaBitstreamError(u32::from(e)));
                    return Err(e);
                }

                (ident, ident_valid) = controller.ident_valid()?;
                ringbuf_entry!(Trace::FrontIOControllerIdent {
                    fpga_id: i,
                    ident
                });

                controller.write_checksum()?;
                (checksum, checksum_valid) = controller.checksum_valid()?;
                ringbuf_entry!(Trace::FrontIOControllerChecksum {
                    fpga_id: i,
                    checksum,
                    expected: FrontIOController::short_checksum(),
                });
            }

            controllers_ready &= ident_valid & checksum_valid;
        }

        Ok(controllers_ready)
    }

    fn leds_set_reset(&self, assert: bool) -> Result<(), FrontIOError> {
        let op = match assert {
            true => WriteOp::BitSet,
            false => WriteOp::BitClear,
        };

        for c in &self.controllers {
            c.user_design
                .write(op, Addr::LED_CTRL, Reg::LED_CTRL::RESET)
                .map_err(FrontIOError::from)?;
        }

        Ok(())
    }

    fn set_system_led_state(&mut self, state: LedState) {
        // TODO: Add a ringbuf entry?
        self.system_led_state = state;
    }

    fn update_leds(&mut self) {
        // handle port LEDs
        let mut next_state = LogicalPortMask(0);
        for (i, state) in self.led_states.into_iter().enumerate() {
            let i = LogicalPort(i as u8);
            match state {
                LedState::On => next_state.set(i),
                LedState::Blink => {
                    if self.led_blink_on {
                        next_state.set(i)
                    }
                }
                LedState::Off => (),
            }
        }
        if let Err(e) = self.leds.update_led_state(next_state) {
            ringbuf_entry!(Trace::LEDUpdateError(e));
        }
        // handle system LED
        let system_led_on = match self.system_led_state {
            LedState::On => true,
            LedState::Blink => self.led_blink_on,
            LedState::Off => false,
        };
        if let Err(e) = self.leds.update_system_led_state(system_led_on) {
            ringbuf_entry!(Trace::LEDUpdateError(e));
        }
    }

    fn handle_i2c_loop(&mut self) {
        if self.leds_initialized {
            self.update_leds();
            let errors = match self.leds.error_summary() {
                Ok(errs) => errs,
                Err(e) => {
                    ringbuf_entry!(Trace::LEDReadError(e));
                    Default::default()
                }
            };
            if errors != self.led_error {
                self.led_error = errors;
                ringbuf_entry!(Trace::LEDErrorSummary(errors));
            }
        } else {
            ringbuf_entry!(Trace::LEDUninitialized);
        }
    }
}

impl idl::InOrderFrontIOImpl for ServerImpl {
    fn board_reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<Infallible>> {
        *self = ServerImpl::default();
        Ok(())
    }

    fn board_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<FrontIOStatus, RequestError<Infallible>> {
        Ok(self.board_status)
    }

    fn board_present(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.board_status != FrontIOStatus::NotPresent)
    }

    fn board_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.board_status == FrontIOStatus::Ready)
    }

    fn phy_osc_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<PhyOscState, RequestError<FrontIOError>> {
        self.phy_smi
            .osc_state()
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)
    }

    fn phy_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<FrontIOError>> {
        self.phy_smi
            .powered_up_and_ready()
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)
    }

    fn phy_set_osc_state(
        &mut self,
        _: &RecvMessage,
        good: bool,
    ) -> Result<(), RequestError<FrontIOError>> {
        match self
            .phy_smi
            .osc_state()
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)?
        {
            // The state of the oscillator has not yet been examined or was
            // marked bad in the previous run. Update as appropriate.
            PhyOscState::Unknown | PhyOscState::Bad => {
                ringbuf_entry!(if good {
                    Trace::PhyOscGood
                } else {
                    Trace::PhyOscBad
                });

                self.phy_smi
                    .set_osc_good(good)
                    .map_err(FrontIOError::from)
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

    fn phy_enable_power(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.phy_smi
            .set_phy_power_enabled(true)
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)
    }

    fn phy_disable_power(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.phy_smi
            .set_phy_power_enabled(false)
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)
    }

    fn phy_set_coma_mode(
        &mut self,
        _: &RecvMessage,
        asserted: bool,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.phy_smi
            .set_coma_mode(asserted)
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)
    }

    fn phy_read(
        &mut self,
        _: &RecvMessage,
        phy: u8,
        reg: u8,
    ) -> Result<u16, RequestError<FrontIOError>> {
        self.phy_smi
            .read_raw(phy, reg)
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)
    }

    fn phy_write(
        &mut self,
        _: &RecvMessage,
        phy: u8,
        reg: u8,
        value: u16,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.phy_smi
            .write_raw(phy, reg, value)
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)
    }

    /// Apply reset to the LED controller
    ///
    /// Per section 7.6 of the datasheet the minimum required pulse width here
    /// is 2.5 microseconds. Given the SPI interface runs at 3MHz, the
    /// transaction to clear the reset would take ~10 microseconds on its own,
    /// so there is no additional delay here.
    fn leds_assert_reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.leds_set_reset(true).map_err(RequestError::from)
    }

    /// Remove reset from the LED controller
    ///
    /// Per section 7.6 of the datasheet the device has a maximum wait time of
    /// 1.5 milliseconds after the release of reset to normal operation, so
    /// there is a 2 millisecond wait here.
    fn leds_deassert_reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.leds_set_reset(false).map_err(RequestError::from)?;
        userlib::hl::sleep_for(2);
        Ok(())
    }

    /// Releases the LED controller from reset and enables the output
    fn leds_enable(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.leds_set_reset(false).map_err(RequestError::from)?;
        for c in &self.controllers {
            c.user_design
                .write(WriteOp::BitSet, Addr::LED_CTRL, Reg::LED_CTRL::OE)
                .map_err(FrontIOError::from)
                .map_err(RequestError::from)?;
        }

        // Once we've initialized the LED driver we do not need to do so again
        if !self.leds_initialized {
            match self.leds.initialize_current() {
                Ok(_) => {
                    self.set_system_led_state(LedState::On);
                    self.leds_initialized = true;
                    ringbuf_entry!(Trace::LEDInitComplete);
                }
                Err(e) => {
                    ringbuf_entry!(Trace::LEDInitError(e));
                    return Err(RequestError::from(
                        FrontIOError::LedInitFailure,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Asserts the LED controller reset and disables the output
    fn leds_disable(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.leds_set_reset(true).map_err(RequestError::from)?;
        for c in &self.controllers {
            c.user_design
                .write(WriteOp::BitClear, Addr::LED_CTRL, Reg::LED_CTRL::OE)
                .map_err(FrontIOError::from)
                .map_err(RequestError::from)?;
        }
        Ok(())
    }

    fn led_set_state(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
        state: LedState,
    ) -> Result<(), RequestError<Infallible>> {
        self.led_states.set(mask, state);
        Ok(())
    }

    fn led_get_state(
        &mut self,
        _: &RecvMessage,
        port: LogicalPort,
    ) -> Result<LedState, RequestError<Infallible>> {
        Ok(self.led_states.get(port))
    }

    fn led_get_system_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<LedState, RequestError<Infallible>> {
        Ok(self.system_led_state)
    }

    fn led_set_system_on(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        self.set_system_led_state(LedState::On);
        Ok(())
    }

    fn led_set_system_off(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        self.set_system_led_state(LedState::Off);
        Ok(())
    }

    fn led_set_system_blink(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        self.set_system_led_state(LedState::Blink);
        Ok(())
    }

    /// Get the current status of all modules
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// TransceiverStatus to be consumed or ignored by the caller.
    fn transceivers_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<TransceiverStatus, RequestError<Infallible>> {
        let (status, result) = self.transceivers.get_module_status();

        Ok(TransceiverStatus { status, result })
    }

    /// Enable power for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_enable_power(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        Ok(self.transceivers.enable_power(mask))
    }

    /// Disable power for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_disable_power(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        Ok(self.transceivers.disable_power(mask))
    }

    /// Clear a fault for each port per the specified `mask`.
    ///
    /// The meaning of the
    /// returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_clear_power_fault(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        Ok(self.transceivers.clear_power_fault(mask))
    }

    /// Assert reset for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_assert_reset(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        Ok(self.transceivers.assert_reset(mask))
    }

    /// Deassert reset for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_deassert_reset(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        Ok(self.transceivers.deassert_reset(mask))
    }

    /// Assert LpMode for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_assert_lpmode(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        Ok(self.transceivers.assert_lpmode(mask))
    }

    /// Deassert LpMode for modules in `mask`
    ///
    /// This operation is considered infallible because the error cases are
    /// handled by the transceivers crate, which then passes back a
    /// ModuleResultNoFailure to be consumed or ignored by the caller. The
    /// meaning of the returned `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_deassert_lpmode(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<Infallible>> {
        Ok(self.transceivers.deassert_lpmode(mask))
    }

    /// Initiate an I2C random read on all ports per the specified `mask`.
    ///
    /// The maximum value of `num_bytes` is 128. This operation is considered
    /// infallible because the error cases are handled by the transceivers
    /// crate, which then passes back a ModuleResultNoFailure to be consumed or
    /// ignored by the caller. The meaning of the returned
    /// `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_setup_i2c_read(
        &mut self,
        _: &RecvMessage,
        reg: u8,
        num_bytes: u8,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<FrontIOError>> {
        if usize::from(num_bytes) > PAGE_SIZE_BYTES {
            return Err(FrontIOError::InvalidNumberOfBytes.into());
        }
        Ok(self.transceivers.setup_i2c_op(true, reg, num_bytes, mask))
    }

    /// Initiate an I2C write on all ports per the specified `mask`.
    ///
    /// The maximum value of `num_bytes` is 128.This operation is considered
    /// infallible because the error cases are handled by the transceivers
    /// crate, which then passes back a ModuleResultNoFailure to be consumed or
    /// ignored by the caller. The meaning of the returned
    /// `ModuleResultNoFailure`:
    /// success: we were able to write to the FPGA
    /// error: an `FpgaError` occurred
    fn transceivers_setup_i2c_write(
        &mut self,
        _msg: &userlib::RecvMessage,
        reg: u8,
        num_bytes: u8,
        mask: LogicalPortMask,
    ) -> Result<ModuleResultNoFailure, RequestError<FrontIOError>> {
        if usize::from(num_bytes) > PAGE_SIZE_BYTES {
            return Err(FrontIOError::InvalidNumberOfBytes.into());
        }
        Ok(self.transceivers.setup_i2c_op(false, reg, num_bytes, mask))
    }

    fn transceivers_get_i2c_status_and_read_buffer(
        &mut self,
        _: &RecvMessage,
        port: LogicalPort,
        dest: Leased<W, [u8]>,
    ) -> Result<PortI2CStatus, RequestError<FrontIOError>> {
        if port.0 >= NUM_PORTS {
            return Err(FrontIOError::InvalidPortNumber.into());
        }

        if dest.len() > PAGE_SIZE_BYTES {
            return Err(FrontIOError::InvalidNumberOfBytes.into());
        }

        // PAGE_SIZE_BYTES + 1 since we have a status byte alongside data
        let mut buf = [0u8; PAGE_SIZE_BYTES + 1];

        let status = self
            .transceivers
            .get_i2c_status_and_read_buffer(port, &mut buf[..dest.len()])
            .map_err(FrontIOError::from)?;

        dest.write_range(0..dest.len(), &buf[..dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        Ok(status)
    }

    fn transceivers_set_i2c_write_buffer(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
        data: Leased<R, [u8]>,
    ) -> Result<ModuleResultNoFailure, RequestError<FrontIOError>> {
        if data.len() > PAGE_SIZE_BYTES {
            return Err(FrontIOError::InvalidNumberOfBytes.into());
        }

        let mut buf = [0u8; PAGE_SIZE_BYTES];
        data.read_range(0..data.len(), &mut buf[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(self.transceivers.set_i2c_write_buffer(&buf, mask))
    }

    fn transceivers_wait_and_check_i2c(
        &mut self,
        _: &RecvMessage,
        mask: LogicalPortMask,
    ) -> Result<ModuleResult, RequestError<Infallible>> {
        Ok(self.transceivers.wait_and_check_i2c(mask))
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {}
}

impl Default for ServerImpl {
    fn default() -> Self {
        let i2c_task = I2C.get_task_id();
        let fpga_task = FRONT_IO.get_task_id();
        let auxflash_task = AUXFLASH.get_task_id();

        ServerImpl {
            auxflash_task,
            controllers: [
                FrontIOController::new(fpga_task, 0),
                FrontIOController::new(fpga_task, 1),
            ],
            leds: Leds::new(
                &i2c_config::devices::pca9956b_front_leds_left(i2c_task),
                &i2c_config::devices::pca9956b_front_leds_right(i2c_task),
            ),
            phy_smi: PhySmi::new(fpga_task),
            transceivers: Transceivers::new(fpga_task),
            board_status: FrontIOStatus::NotPresent,
            led_blink_on: false,
            led_error: Default::default(),
            leds_initialized: false,
            led_states: LedStates::default(),
            system_led_state: LedState::Off,
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let mut server = ServerImpl::default();

    #[derive(Copy, Clone, Enum)]
    #[allow(clippy::upper_case_acronyms)]
    enum Timers {
        I2C,
        Blink,
        Seq,
    }
    let mut multitimer = Multitimer::<Timers>::new(notifications::TIMER_BIT);
    let now = sys_get_timer().now;
    multitimer.set_timer(
        Timers::I2C,
        now,
        Some(Repeat::AfterDeadline(I2C_INTERVAL)),
    );
    multitimer.set_timer(
        Timers::Blink,
        now,
        Some(Repeat::AfterDeadline(BLINK_INTERVAL)),
    );
    multitimer.set_timer(
        Timers::Seq,
        now,
        Some(Repeat::AfterDeadline(SEQ_INTERVAL)),
    );

    // This will put our timer in the past, and should immediately kick us.
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    loop {
        multitimer.poll_now();
        for t in multitimer.iter_fired() {
            match t {
                Timers::I2C => {
                    // There's no point to try to talk to the I2C bus if a board
                    // is not present.
                    if server.board_status != FrontIOStatus::NotPresent {
                        server.handle_i2c_loop();
                    }
                }
                Timers::Blink => {
                    server.led_blink_on = !server.led_blink_on;
                }
                Timers::Seq => {
                    ringbuf_entry!(Trace::SequenceUpdate(server.board_status));

                    // Sequencing of the Front IO board
                    match server.board_status {
                        // The best way we have to detect the presence of a
                        // Front IO board is our ability to talk to its FRUID
                        // device.
                        FrontIOStatus::NotPresent => {
                            let fruid =
                                i2c_config::devices::at24csw080_front_io(
                                    I2C.get_task_id(),
                                )[0];
                            if At24Csw080::validate(&fruid).unwrap_or(false) {
                                server.board_status = FrontIOStatus::FpgaInit;
                            }
                        }

                        // Once there is a board present, configure its FPGAs
                        // and wait for its oscillator to be functional.
                        FrontIOStatus::FpgaInit => match server.fpga_init() {
                            Ok(done) => {
                                if done && server.initialized() {
                                    server.board_status =
                                        FrontIOStatus::OscInit;
                                }
                            }
                            Err(e) => ringbuf_entry!(Trace::FpgaInitError(e)),
                        },

                        // Wait for the PHY oscillator to be deemed operational.
                        // Currently this server does not control the power to
                        // the Front IO board, so it relies on whatever task
                        // _does_ have that control to power cycle the board and
                        // make a judgement about the oscillator.
                        FrontIOStatus::OscInit => {
                            if server
                                .phy_smi
                                .osc_state()
                                .unwrap_or(PhyOscState::Unknown)
                                == PhyOscState::Good
                            {
                                server.board_status = FrontIOStatus::Ready;
                            }
                        }

                        // The board is operational, not further action needed
                        FrontIOStatus::Ready => (),
                    }
                }
            }
        }

        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{
        FrontIOError, FrontIOStatus, LedState, LogicalPort, LogicalPortMask,
        ModuleResult, ModuleResultNoFailure, PhyOscState, PortI2CStatus,
        TransceiverStatus,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
