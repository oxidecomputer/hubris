// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Medusa sequencing process.

#![no_std]
#![no_main]

use crate::front_io::FrontIOBoard;
use crate::power_control::PowerControl;
use core::convert::Infallible;
use drv_medusa_seq_api::{MedusaError, RailName};
use drv_sidecar_front_io::phy_smi::PhyOscState;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(AUXFLASH, auxflash);
task_slot!(PACKRAT, packrat);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

mod front_io;
mod power_control;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    FpgaBitstreamError(u32),
    FrontIOBoardNotPresent,
    FrontIOBoardPresent,
    FrontIOBoardPowerEnable(bool),
    FrontIOBoardPowerGood,
    FrontIOBoardPowerFault,
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
    PowerEnable(RailName, bool),
    PowerFault(RailName),
    MgmtPowerGood,
    PhyPowerGood,
}

ringbuf!(Trace, 32, Trace::None);

const TIMER_INTERVAL: u64 = 1000;

struct ServerImpl {
    power_control: PowerControl,
    front_io_board: Option<FrontIOBoard>,
}

impl ServerImpl {
    fn front_io_board_preinit(&self) -> Result<bool, MedusaError> {
        // Enable the V12_QSFP_OUT rail
        self.power_control.v12_qsfp_out.set_enable(true);

        // Wait a bit for it to ramp and then check that it is happy.
        // The EN->PG time for this part was experimentally determined to be
        // 35ms, so we roughly double that.
        userlib::hl::sleep_for(75);

        // Power is not good. Disable the rail and log that this happened.
        if !self.power_control.v12_qsfp_out.check_power_good() {
            return Err(MedusaError::FrontIOBoardPowerFault);
        }

        // Determine if a front IO board is present.
        Ok(FrontIOBoard::present(I2C.get_task_id()))
    }

    fn actually_reset_front_io_phy(&mut self) -> Result<(), MedusaError> {
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
                        self.power_control.v12_qsfp_out.set_enable(false);
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
                        front_io_board
                            .phy()
                            .set_phy_power_enabled(false)
                            .map_err(MedusaError::from)?;
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

        let front_io_board = self
            .front_io_board
            .as_mut()
            .ok_or(MedusaError::NoFrontIOBoard)?;

        // At this point the front IO board has either not yet been
        // initialized or may have been power cycled and should be
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
    }
}

impl idl::InOrderSequencerImpl for ServerImpl {
    fn control_mgmt_rails(
        &mut self,
        _: &RecvMessage,
        enabled: bool,
    ) -> Result<(), RequestError<MedusaError>> {
        self.power_control.v1p0_mgmt.set_enable(enabled);
        self.power_control.v1p2_mgmt.set_enable(enabled);
        self.power_control.v2p5_mgmt.set_enable(enabled);

        if enabled {
            userlib::hl::sleep_for(10);
            if !self.power_control.mgmt_power_check() {
                return Err(RequestError::from(MedusaError::PowerFault));
            }
            ringbuf_entry!(Trace::MgmtPowerGood);
        }

        Ok(())
    }

    fn control_phy_rails(
        &mut self,
        _: &RecvMessage,
        enabled: bool,
    ) -> Result<(), RequestError<MedusaError>> {
        self.power_control.v1p0_phy.set_enable(enabled);
        self.power_control.v2p5_phy.set_enable(enabled);

        if enabled {
            userlib::hl::sleep_for(10);
            if !self.power_control.phy_power_check() {
                return Err(RequestError::from(MedusaError::PowerFault));
            }
            ringbuf_entry!(Trace::PhyPowerGood);
        }

        Ok(())
    }

    fn control_rail(
        &mut self,
        _: &RecvMessage,
        name: RailName,
        enabled: bool,
    ) -> Result<(), RequestError<Infallible>> {
        let rail = self.power_control.get_rail(name);
        rail.set_enable(enabled);
        Ok(())
    }

    fn front_io_board_present(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.front_io_board.is_some())
    }

    fn set_front_io_phy_osc_state(
        &mut self,
        _: &RecvMessage,
        good: bool,
    ) -> Result<(), RequestError<MedusaError>> {
        let front_io_board = self
            .front_io_board
            .as_ref()
            .ok_or(MedusaError::NoFrontIOBoard)?;

        match front_io_board
            .phy()
            .osc_state()
            .map_err(MedusaError::from)
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
                    .map_err(MedusaError::from)
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

    fn reset_front_io_phy(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<MedusaError>> {
        self.actually_reset_front_io_phy()
            .map_err(RequestError::from)
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let mut server = ServerImpl {
        power_control: PowerControl::new(),
        front_io_board: None,
    };

    // Enable the front IO hot swap controller and probe for a front IO board.
    match server.front_io_board_preinit() {
        Ok(true) => {
            ringbuf_entry!(Trace::FrontIOBoardPresent);
            ringbuf_entry!(Trace::FrontIOBoardPowerGood);

            let mut front_io_board = FrontIOBoard::new(
                FRONT_IO.get_task_id(),
                AUXFLASH.get_task_id(),
            );

            front_io_board.init().unwrap_lite();

            // TODO: check/load VPD data into packrat.

            // So far the front IO board looks functional. Assign it to the
            // server, implicitly marking it present for the lifetime of this
            // task.
            server.front_io_board = Some(front_io_board);
        }
        Ok(false) => {
            ringbuf_entry!(Trace::FrontIOBoardNotPresent);
            server.power_control.v12_qsfp_out.set_enable(false);
        }
        Err(MedusaError::FrontIOBoardPowerFault) => {
            ringbuf_entry!(Trace::FrontIOBoardPowerFault)
        }
        // `front_io_board_preinit` currently only returns a
        // MedusaError::FrontIOBoardPowerFault
        Err(_) => unreachable!(),
    }

    // The MGMT and PHY rails are enabled automatically by pullups, so we will
    // check their power good signals and take action as appropriate.
    if server.power_control.mgmt_power_check() {
        ringbuf_entry!(Trace::MgmtPowerGood);
    }
    if server.power_control.phy_power_check() {
        ringbuf_entry!(Trace::PhyPowerGood);
    }

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{MedusaError, RailName};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
