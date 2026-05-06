// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Front IO board

#![no_std]
#![no_main]

use core::convert::Infallible;
use drv_fpga_api::{DeviceState, FpgaError};
use drv_front_io_api::{FrontIOError, FrontIOStatus, controller::FrontIOController};
use drv_i2c_devices::{at24csw080::At24Csw080, Validate};
use drv_sidecar_seq_api::Sequencer;
use enum_map::Enum;
use idol_runtime::{NotificationHandler, RequestError};
use multitimer::{Multitimer, Repeat};
use ringbuf::*;
use userlib::*;


task_slot!(AUXFLASH, auxflash);
task_slot!(FRONT_IO_FPGA, ecp5_front_io);
task_slot!(I2C, i2c_driver);
task_slot!(SEQUENCER, sequencer);


#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    BspInit,
    BspInitComplete,
    BspInitFailed,
    PowerEnabled(bool),
    PowerGood,
    PowerNotGood,
    PowerFault,
    FpgaInitError(FpgaError),
    PhyPowerEnabled(bool),
    PhyOscGood,
    PhyOscBad,
    LEDInitComplete,
    // LEDInitError(pca9956b::Error),
    // LEDUpdateError(pca9956b::Error),
    // LEDReadError(pca9956b::Error),
    // LEDErrorSummary(FullErrorSummary),
    SeqStatus(FrontIOStatus),
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
    // SystemLedState(LedState),
}
ringbuf!(Trace, 32, Trace::None);

/// Controls how often we update the LED controllers (in milliseconds).
const I2C_INTERVAL: u64 = 100;

/// Blink LEDs at a 50% duty cycle (in milliseconds)
const BLINK_INTERVAL: u64 = 500;

/// How often we should attempt the next sequencing step (in milliseconds)
const SEQ_INTERVAL: u64 = 100;

struct ServerImpl {
    seq: Sequencer,

    /// Handle for the auxflash task
    auxflash_task: userlib::TaskId,

    /// Handles for each FPGA
    controllers: [FrontIOController; 2],

    /// Status of the Front IO board
    board_status: FrontIOStatus,

    /// State around LED management
    led_blink_on: bool,
    // led_error: FullErrorSummary,
    // leds_initialized: bool,
    // led_states: LedStates,
    // system_led_state: LedState,
}

impl ServerImpl {
    // We don't have a good way to tell if the board is present purely electrically, so instead we
    // rely on our ability to talk to the board's FRUID as a proxy for presence + power good
    fn is_board_present_and_powered(&self) -> bool {
        let fruid =
            i2c_config::devices::at24csw080_front_io(I2C.get_task_id())[0];
        At24Csw080::validate(&fruid).unwrap_or(false)
    }

    // Encapsulates the logic of verifying checksums and loading FPGA images as necessary with the
    // goal of only reloading FPGAs when there are new images in order to preserve state.
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

    // Helper function to query if both FPGAs are ready
    fn fpga_ready(&self) -> bool {
        self.controllers.iter().all(|c| c.ready().unwrap_or(false))
    }
}

impl idl::InOrderFrontIOImpl for ServerImpl {
    /// Returns true if a front IO board was determined to be present and powered on
    fn board_present(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.is_board_present_and_powered())
    }

    /// Returns if the front IO board has completely sequenced and is ready
    fn board_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<FrontIOError>> {
        Ok(self.board_status == FrontIOStatus::Ready)
    }

    fn phy_reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.seq
            .reset_front_io_phy()
            .map_err(|_| FrontIOError::SeqError)
            .map_err(RequestError::from)
    }

    /// Set the internal state of the PHY's oscillator
    fn phy_set_osc_state(
        &mut self,
        _: &RecvMessage,
        good: bool,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.seq
            .set_front_io_phy_osc_state(good)
            .map_err(|_| FrontIOError::SeqError)
            .map_err(RequestError::from)
    }
}

// notifications are not supported at this time
impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: NotificationBits) {
        unreachable!()
    }
}

impl Default for ServerImpl {
    fn default() -> Self {
        let fpga_task = FRONT_IO_FPGA.get_task_id();

        ServerImpl {
            seq: Sequencer::from(SEQUENCER.get_task_id()),
            auxflash_task: AUXFLASH.get_task_id(),
            controllers: [
                FrontIOController::new(fpga_task, 0),
                FrontIOController::new(fpga_task, 1),
            ],
            board_status: FrontIOStatus::Init,
            led_blink_on: false,
        }
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
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

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        multitimer.poll_now();
        for t in multitimer.iter_fired() {
            match t {
                Timers::I2C => {
                    // There's no point to try to talk to the I2C bus if a board
                    // is not present.
                    if server.board_status != FrontIOStatus::NotPresent {
                        // server.handle_i2c_loop();
                    }
                }
                Timers::Blink => {
                    server.led_blink_on = !server.led_blink_on;
                }
                Timers::Seq => {
                    // Sequencing of the Front IO board
                    match server.board_status {
                        // The best way we have to detect the presence of a
                        // Front IO board is our ability to talk to its FRUID
                        // device.
                        FrontIOStatus::Init | FrontIOStatus::NotPresent => {
                            ringbuf_entry!(Trace::SeqStatus(
                                server.board_status
                            ));

                            if server.is_board_present_and_powered() {
                                server.board_status = FrontIOStatus::FpgaInit;
                            } else {
                                server.board_status = FrontIOStatus::NotPresent;
                            }
                        }

                        // Once we know there is a board present, configure its
                        // FPGAs and wait for its PHY oscillator to be functional.
                        FrontIOStatus::FpgaInit => {
                            ringbuf_entry!(Trace::SeqStatus(
                                server.board_status
                            ));
                            match server.fpga_init() {
                                Ok(done) => {
                                    if done && server.fpga_ready() {
                                        server.board_status =
                                            FrontIOStatus::OscInit;
                                    }
                                }
                                Err(e) => {
                                    ringbuf_entry!(Trace::FpgaInitError(e))
                                }
                            }
                        }

                         // Wait for the PHY oscillator to be deemed operational.
                        // Currently this server does not control the power to
                        // the Front IO board, so it relies on whatever task
                        // _does_ have that control to power cycle the board and
                        // make a judgement about the oscillator.
                        FrontIOStatus::OscInit => {
                            ringbuf_entry!(Trace::SeqStatus(
                                server.board_status
                            ));
                            // if server
                            //     .phy_smi
                            //     .osc_state()
                            //     .unwrap_or(PhyOscState::Unknown)
                            //     == PhyOscState::Good
                            // {
                            //     server.board_status = FrontIOStatus::Ready;
                            //     ringbuf_entry!(Trace::SeqStatus(
                            //         server.board_status
                            //     ));
                            // }
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
    use super::FrontIOError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));