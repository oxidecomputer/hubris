// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Medusa sequencing process.

#![no_std]
#![no_main]

use crate::power_control::PowerControl;
use core::convert::Infallible;
use drv_front_io_api::FrontIO;
use drv_medusa_seq_api::{MedusaError, RailName};
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::{ringbuf, ringbuf_entry};
use sys_api::{OutputType, Port, Pull, Speed, Sys};
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(FRONT_IO, front_io);
task_slot!(AUXFLASH, auxflash);
task_slot!(PACKRAT, packrat);
task_slot!(SYS, sys);

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

mod power_control;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    FrontIOBoardNotPresent,
    FrontIOBoardPresent,
    FrontIOBoardPowerEnable(bool),
    FrontIOBoardPowerGood,
    FrontIOBoardPowerNotGood,
    FrontIOBoardPhyPowerEnable(bool),
    FrontIOBoardPhyOscGood,
    FrontIOBoardPhyOscBad,
    PowerEnable(RailName, bool),
    PowerFault(RailName),
    MgmtPowerGood,
    FrontPhyPowerGood,
    LocalPhyPowerGood,
}

ringbuf!(Trace, 32, Trace::None);

const TIMER_INTERVAL: u64 = 1000;

struct ServerImpl {
    power_control: PowerControl,
    front_io_board: FrontIO,
    vsc7448_ready: bool,
    local_vsc8562_ready: bool,
    local_vsc8562_coma_mode_pin: sys_api::PinSet,
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

    fn rail_good(
        &mut self,
        _: &RecvMessage,
        name: RailName,
    ) -> Result<bool, RequestError<Infallible>> {
        let rail = self.power_control.get_rail(name);
        Ok(rail.power_good())
    }

    fn vsc7448_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.vsc7448_ready)
    }

    fn local_vsc8562_ready(
        &mut self,
        _: &RecvMessage,
    ) -> Result<bool, RequestError<Infallible>> {
        Ok(self.local_vsc8562_ready)
    }

    fn set_local_phy_coma_mode(
        &mut self,
        _: &RecvMessage,
        asserted: bool,
    ) -> Result<(), RequestError<Infallible>> {
        let sys = Sys::from(SYS.get_task_id());
        sys.gpio_set_to(self.local_vsc8562_coma_mode_pin, asserted);
        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        let next_deadline = sys_get_timer().now + TIMER_INTERVAL;

        sys_set_timer(Some(next_deadline), notifications::TIMER_MASK);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; idl::INCOMING_SIZE];

    let mut server = ServerImpl {
        power_control: PowerControl::new(),
        front_io_board: FrontIO::from(FRONT_IO.get_task_id()),
        vsc7448_ready: false,
        local_vsc8562_ready: false,
        local_vsc8562_coma_mode_pin: Port::I.pin(10),
    };

    let sys = Sys::from(SYS.get_task_id());

    // PE11 has an external 1K pull down
    let vsc7448_reset_l_pin = Port::E.pin(11);
    sys.gpio_configure_output(
        vsc7448_reset_l_pin,
        OutputType::PushPull,
        Speed::Low,
        Pull::None,
    );
    sys.gpio_set_to(vsc7448_reset_l_pin, false);

    // PI9 has an external 1K pull down
    let local_phy_reset_l_pin = Port::I.pin(9);
    sys.gpio_configure_output(
        local_phy_reset_l_pin,
        OutputType::PushPull,
        Speed::Low,
        Pull::None,
    );
    sys.gpio_set_to(local_phy_reset_l_pin, false);

    // PI10 does not have an external strapping on the SP-side of a voltage translator
    sys.gpio_configure_output(
        server.local_vsc8562_coma_mode_pin,
        OutputType::PushPull,
        Speed::Low,
        Pull::Up,
    );
    sys.gpio_set_to(server.local_vsc8562_coma_mode_pin, true);

    // Enable VSC7448 (mgmt) and VSC8562 (phy) rails
    server.power_control.v1p0_mgmt.set_enable(true);
    server.power_control.v1p2_mgmt.set_enable(true);
    server.power_control.v2p5_mgmt.set_enable(true);
    // both sets of phy rails share an enable, so no need to explicitly enable the 2.5V rail
    server.power_control.v1p0_front_phy.set_enable(true);
    server.power_control.v1p0_local_phy.set_enable(true);

    // Enable the front IO hot swap controller and probe for a front IO board.
    match server.front_io_board.power_on() {
        Ok(_) => {
            if server.front_io_board.board_present() {
                ringbuf_entry!(Trace::FrontIOBoardPresent);
                ringbuf_entry!(Trace::FrontIOBoardPowerGood);
                // TODO: check/load VPD data into packrat.
            } else {
                ringbuf_entry!(Trace::FrontIOBoardNotPresent);
                server.front_io_board.set_power_enable(false).unwrap_lite();
            }
        }
        // Something went wrong getting the HSC status, eject.
        Err(_) => {
            ringbuf_entry!(Trace::FrontIOBoardPowerNotGood);
            server.front_io_board.set_power_enable(false).unwrap_lite();
        }
    }

    // The MGMT and PHY rails were previously enabled, so next we will
    // check their power good signals and take action as appropriate.
    if server.power_control.mgmt_power_check() {
        ringbuf_entry!(Trace::MgmtPowerGood);
        // power is good, release reset
        sys.gpio_set_to(vsc7448_reset_l_pin, true);
        // the VSC7448 needs at most 50ms to be ready after the reset
        userlib::hl::sleep_for(50);
        server.vsc7448_ready = true;
    }

    if server.power_control.front_phy_power_check() {
        ringbuf_entry!(Trace::FrontPhyPowerGood);
    }
    if server.power_control.local_phy_power_check() {
        ringbuf_entry!(Trace::LocalPhyPowerGood);
        // power is good, release reset
        sys.gpio_set_to(local_phy_reset_l_pin, true);
        // Wait for the chip to come out of reset
        userlib::hl::sleep_for(120);
        server.local_vsc8562_ready = true;
    }

    // This will put our timer in the past, and should immediately kick us.
    let deadline = sys_get_timer().now;
    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

mod idl {
    use super::{MedusaError, RailName};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
