// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Medusa sequencing process.

#![no_std]
#![no_main]

use crate::power_control::PowerControl;
use core::convert::Infallible;
use drv_front_io_api::{FrontIO, FrontIOError};
use drv_medusa_seq_api::{MedusaError, RailName};
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(AUXFLASH, auxflash);
task_slot!(PACKRAT, packrat);
task_slot!(SYS, sys);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

mod power_control;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    FrontIOBoardNotPresent,
    FrontIOBoardPresent,
    FrontIOBoardPowerGood,
    FrontIOBoardPowerNotGood,
    PowerEnable(RailName, bool),
    PowerFault(RailName),
    MgmtPowerGood,
    PhyPowerGood,
}

ringbuf!(Trace, 32, Trace::None);

struct ServerImpl {
    power_control: PowerControl,
    front_io_board: FrontIO,
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
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let mut server = ServerImpl {
        power_control: PowerControl::new(),
        front_io_board: FrontIO::from(FRONT_IO.get_task_id()),
    };

    // Enable the front IO hot swap controller and probe for a front IO board.
    match server.front_io_board.power_on() {
        Ok(_) => {
            if server.front_io_board.board_present() {
                ringbuf_entry!(Trace::FrontIOBoardPresent);
                ringbuf_entry!(Trace::FrontIOBoardPowerGood);
                // TODO: check/load VPD data into packrat.
            } else {
                ringbuf_entry!(Trace::FrontIOBoardNotPresent)
            }
        }
        Err(FrontIOError::PowerFault | FrontIOError::PowerNotGood) => {
            ringbuf_entry!(Trace::FrontIOBoardPowerNotGood)
        }
        // Something went wrong getting the HSC status, eject.
        Err(_) => panic!("unknown front IO board preinit failure"),
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
