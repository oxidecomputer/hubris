// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Front IO board

#![no_std]
#![no_main]

#[cfg_attr(
    any(
        target_board = "sidecar-b",
        target_board = "sidecar-c",
        target_board = "sidecar-d"
    ),
    path = "bsp/sidecar_bcd.rs"
)]
#[cfg_attr(target_board = "medusa-a", path = "bsp/medusa_a.rs")]
mod bsp;

use crate::bsp::Bsp;
use core::convert::Infallible;
use drv_fpga_api::{DeviceState, FpgaError};
use drv_fpga_user_api::power_rail::PowerRailStatus;
use drv_front_io_api::{
    FrontIOError, FrontIOStatus,
    controller::FrontIOController,
    phy_smi::{PhyOscState, PhySmi},
};
use drv_i2c_devices::{Validate, at24csw080::At24Csw080};
use enum_map::Enum;
use idol_runtime::{NotificationHandler, RequestError};
use multitimer::{Multitimer, Repeat};
use ringbuf::*;
use userlib::*;

task_slot!(AUXFLASH, auxflash);
task_slot!(FRONT_IO_FPGA, ecp5_front_io);
task_slot!(I2C, i2c_driver);

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
}
ringbuf!(Trace, 32, Trace::None);

/// How often we should attempt the next sequencing step (in milliseconds)
const SEQ_INTERVAL: u64 = 100;

struct ServerImpl {
    /// A BSP to help deliver core functionality whose implementation varies from board to board
    bsp: Bsp,

    /// Handle for the auxflash task
    auxflash_task: userlib::TaskId,

    /// Handles for each FPGA
    controllers: [FrontIOController; 2],

    /// VSC8562 SMI Interface
    phy_smi: PhySmi,

    /// Status of the Front IO board
    board_status: FrontIOStatus,
}

impl ServerImpl {
    // We don't have a good way to tell if the board is present purely electrically, so instead we
    // rely on our ability to talk to the board's FRUID as a proxy for presence + power good
    fn is_board_present_and_powered(&self) -> bool {
        let fruid =
            i2c_config::devices::at24csw080_front_io(I2C.get_task_id())[0];
        At24Csw080::validate(&fruid).unwrap_or(false)
    }

    // Make sure the front IO hot swap controller is enabled and good.
    fn power_on_check(&self) -> Result<(), FrontIOError> {
        // power rail FSM will reach either the GoodTimeout, Aborted or Enabled
        // state or experience an FpgaError, so an open loop is safe.
        while match self.bsp.power_rail_status()? {
            PowerRailStatus::GoodTimeout | PowerRailStatus::Aborted => {
                ringbuf_entry!(Trace::PowerFault);
                return Err(FrontIOError::PowerFault);
            }
            PowerRailStatus::Disabled => {
                ringbuf_entry!(Trace::PowerEnabled(true));
                self.bsp.set_power_enable(true)?;
                true // Retry HSC status.
            }
            PowerRailStatus::RampingUp => {
                true // Retry HSC status.
            }
            PowerRailStatus::Enabled => false,
        } {
            // The front IO HSC was observed to take as long as 35 ms to assert power good
            userlib::hl::sleep_for(100);
        }

        // Check if the power is good via the PG pin
        if self.bsp.power_good() {
            ringbuf_entry!(Trace::PowerGood);
        } else {
            ringbuf_entry!(Trace::PowerNotGood);
            return Err(FrontIOError::PowerNotGood);
        }

        Ok(())
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

    fn do_server_reset(&mut self) {
        *self = ServerImpl::default();
    }
}

impl idl::InOrderFrontIOImpl for ServerImpl {
    /// Combines turning the front IO board power on and checking that it is good
    fn power_on(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<FrontIOError>> {
        self.power_on_check().map_err(RequestError::from)
    }

    /// Returns the current status of the front IO board
    fn board_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<FrontIOStatus, RequestError<Infallible>> {
        Ok(self.board_status)
    }

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
        match self
            .phy_smi
            .osc_state()
            .map_err(FrontIOError::from)
            .map_err(RequestError::from)?
        {
            PhyOscState::Bad => {
                // The PHY was attempted to be initialized but its
                // oscillator was deemed not functional. Unfortunately
                // the only course of action is to power cycle the
                // entire front IO board, so do so now.
                ringbuf_entry!(Trace::PowerEnabled(false));
                self.bsp.set_power_enable(false)?;
                // After removing power to the board we must reset its
                // server
                self.do_server_reset();

                // Wait some cool down period to allow caps to bleed off
                // etc.
                userlib::hl::sleep_for(1000);
            }
            PhyOscState::Good => {
                // The PHY was initialized properly before and its
                // oscillator declared operating nominally. Assume this
                // has not changed and only a reset the PHY itself is
                // desired.
                self.phy_smi
                    .set_phy_power_enabled(false)
                    .map_err(FrontIOError::from)
                    .map_err(RequestError::from)?;
                ringbuf_entry!(Trace::PhyPowerEnabled(false));

                userlib::hl::sleep_for(10);
            }
            PhyOscState::Unknown => {
                // Do nothing (yet) since the oscillator state is
                // unknown.
            }
        }

        // Handle re-enabling power as needed
        self.power_on_check().map_err(RequestError::from)?;

        if self.is_board_present_and_powered() {
            // At this point the front IO board has either not yet been
            // initalized or may have been power cycled and should be
            // initialized.
            while !self.fpga_ready() {
                userlib::hl::sleep_for(20);
            }

            // The PHY is still powered down. Request the sequencer to power up
            // and wait for it to be ready.
            self.phy_smi
                .set_phy_power_enabled(true)
                .map_err(FrontIOError::from)
                .map_err(RequestError::from)?;
            ringbuf_entry!(Trace::PhyPowerEnabled(true));

            while !self
                .phy_smi
                .powered_up_and_ready()
                .map_err(FrontIOError::from)
                .map_err(RequestError::from)?
            {
                userlib::hl::sleep_for(20);
            }

            Ok(())
        } else {
            Err(RequestError::from(FrontIOError::NotPresent))
        }
    }

    /// Set the internal state of the PHY's oscillator
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
        ringbuf_entry!(Trace::BspInit);
        let bsp = match Bsp::new() {
            Ok(bsp) => {
                ringbuf_entry!(Trace::BspInitComplete);
                bsp
            }
            Err(_) => {
                ringbuf_entry!(Trace::BspInitFailed);
                panic!();
            }
        };

        let fpga_task = FRONT_IO_FPGA.get_task_id();

        ServerImpl {
            bsp,
            auxflash_task: AUXFLASH.get_task_id(),
            controllers: [
                FrontIOController::new(fpga_task, 0),
                FrontIOController::new(fpga_task, 1),
            ],
            phy_smi: PhySmi::new(fpga_task),
            board_status: FrontIOStatus::Init,
        }
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let mut server = ServerImpl::default();

    // TODO: there will be more timers when I2C gets moved into this server
    #[derive(Copy, Clone, Enum)]
    #[allow(clippy::upper_case_acronyms)]
    enum Timers {
        Seq,
    }
    let mut multitimer = Multitimer::<Timers>::new(notifications::TIMER_BIT);
    let now = sys_get_timer().now;
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
                            if server
                                .phy_smi
                                .osc_state()
                                .unwrap_or(PhyOscState::Unknown)
                                == PhyOscState::Good
                            {
                                server.board_status = FrontIOStatus::Ready;
                                ringbuf_entry!(Trace::SeqStatus(
                                    server.board_status
                                ));
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
    use super::{FrontIOError, FrontIOStatus};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
