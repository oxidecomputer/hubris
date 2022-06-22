// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::bsp::{self, Bsp};
use idol_runtime::{NotificationHandler, RequestError};
use monorail_api::{MonorailError, PortDev, PortStatus, VscError};
use userlib::{sys_get_timer, sys_set_timer};
use vsc7448::{config::PortMap, Vsc7448, Vsc7448Rw};
use vsc7448_pac::{types::PhyRegisterAddress, *};

pub struct ServerImpl<'a, R> {
    bsp: Bsp<'a, R>,
    vsc7448: &'a Vsc7448<'a, R>,
    map: &'a PortMap,
    wake_target_time: u64,
}

/// Notification mask for optional periodic logging
pub const WAKE_IRQ: u32 = 1;
pub const INCOMING_SIZE: usize = idl::INCOMING_SIZE;

impl<'a, R: Vsc7448Rw> ServerImpl<'a, R> {
    pub fn new(
        bsp: Bsp<'a, R>,
        vsc7448: &'a Vsc7448<'a, R>,
        map: &'a PortMap,
    ) -> Self {
        // Some of the BSPs include a 'wake' function which allows for periodic
        // logging.  We schedule a wake-up before entering the idol_runtime dispatch
        // loop, to make sure that this gets called periodically.
        let wake_target_time = sys_get_timer().now;
        sys_set_timer(Some(0), WAKE_IRQ); // Trigger a wake IRQ right away
        Self {
            bsp,
            wake_target_time,
            map,
            vsc7448,
        }
    }

    pub fn wake(&mut self) -> Result<(), VscError> {
        let now = sys_get_timer().now;
        if let Some(wake_interval) = bsp::WAKE_INTERVAL {
            if now >= self.wake_target_time {
                let out = self.bsp.wake();
                self.wake_target_time += wake_interval;
                sys_set_timer(Some(self.wake_target_time), WAKE_IRQ);
                return out;
            }
        }
        Ok(())
    }
}

impl<'a, R: Vsc7448Rw> idl::InOrderMonorailImpl for ServerImpl<'a, R> {
    fn get_port_status(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
    ) -> Result<PortStatus, RequestError<MonorailError>> {
        if usize::from(port) >= self.map.len() {
            return Err(MonorailError::InvalidPort.into());
        }
        let cfg = match self.map.port_config(port) {
            None => return Err(MonorailError::UnconfiguredPort.into()),
            Some(cfg) => cfg,
        };
        let link_up = match cfg.dev.0 {
            PortDev::Dev1g => {
                self.vsc7448
                    .read(
                        DEV1G(cfg.dev.1).PCS1G_CFG_STATUS().PCS1G_LINK_STATUS(),
                    )
                    .map_err(MonorailError::from)?
                    .link_status()
                    != 0
            }
            PortDev::Dev2g5 => {
                self.vsc7448
                    .read(
                        DEV2G5(cfg.dev.1)
                            .PCS1G_CFG_STATUS()
                            .PCS1G_LINK_STATUS(),
                    )
                    .map_err(MonorailError::from)?
                    .link_status()
                    != 0
            }
            PortDev::Dev10g => {
                // Section of 3.8.2.2 describes how to monitor link status for
                // DEV10G, which isn't as simple as the DEV1G/2G5.
                self.vsc7448
                    .read(PCS10G_BR(cfg.dev.1).PCS_10GBR_STATUS().PCS_STATUS())
                    .map_err(MonorailError::from)?
                    .rx_block_lock()
                    != 0
            }
        };
        Ok(PortStatus { cfg, link_up })
    }

    fn read_phy_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
        page: u16,
        reg: u8,
    ) -> Result<u16, RequestError<MonorailError>> {
        if usize::from(port) >= self.map.len() {
            return Err(MonorailError::InvalidPort.into());
        } else if self.map.port_config(port).is_none() {
            return Err(MonorailError::UnconfiguredPort.into());
        }
        let addr = PhyRegisterAddress::from_page_and_addr_unchecked(page, reg);
        match self.bsp.phy_fn(port, |phy| phy.read(addr)) {
            None => return Err(MonorailError::NoPhy.into()),
            Some(r) => {
                r.map_err(MonorailError::from).map_err(RequestError::from)
            }
        }
    }

    fn write_phy_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
        page: u16,
        reg: u8,
        value: u16,
    ) -> Result<(), RequestError<MonorailError>> {
        if usize::from(port) >= self.map.len() {
            return Err(MonorailError::InvalidPort.into());
        } else if self.map.port_config(port).is_none() {
            return Err(MonorailError::UnconfiguredPort.into());
        }
        let addr = PhyRegisterAddress::from_page_and_addr_unchecked(page, reg);
        match self.bsp.phy_fn(port, |phy| phy.write(addr, value)) {
            None => return Err(MonorailError::NoPhy.into()),
            Some(r) => {
                r.map_err(MonorailError::from).map_err(RequestError::from)
            }
        }
    }

    fn read_vsc7448_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        addr: u32,
    ) -> Result<u32, RequestError<MonorailError>> {
        let addr =
            vsc7448_pac::types::RegisterAddress::<u32>::from_addr_unchecked(
                addr,
            );
        self.vsc7448
            .read(addr)
            .map_err(MonorailError::from)
            .map_err(RequestError::from)
    }

    fn write_vsc7448_reg(
        &mut self,
        _msg: &userlib::RecvMessage,
        addr: u32,
        value: u32,
    ) -> Result<(), RequestError<MonorailError>> {
        let addr =
            vsc7448_pac::types::RegisterAddress::<u32>::from_addr_unchecked(
                addr,
            );
        self.vsc7448
            .write(addr, value)
            .map_err(MonorailError::from)
            .map_err(RequestError::from)
    }
}

impl<'a, R> NotificationHandler for ServerImpl<'a, R> {
    fn current_notification_mask(&self) -> u32 {
        // We're always listening for the wake (timer) irq
        WAKE_IRQ
    }

    fn handle_notification(&mut self, _bits: u32) {
        // Nothing to do here: the wake IRQ is handled in the main `net` loop
    }
}

mod idl {
    use super::{MonorailError, PortStatus};
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
