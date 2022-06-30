// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::bsp::{self, Bsp};
use idol_runtime::{NotificationHandler, RequestError};
use monorail_api::{
    MonorailError, PacketCount, PhyStatus, PhyType, PortCounters, PortDev,
    PortStatus, VscError,
};
use userlib::{sys_get_timer, sys_set_timer};
use vsc7448::{config::PortMap, DevGeneric, Vsc7448, Vsc7448Rw};
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

    /// Helper function to return an error if a user-specified port is invalid
    fn check_port(&self, port: u8) -> Result<(), MonorailError> {
        if usize::from(port) >= self.map.len() {
            Err(MonorailError::InvalidPort)
        } else if self.map.port_config(port).is_none() {
            Err(MonorailError::UnconfiguredPort)
        } else {
            Ok(())
        }
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
            // These devices use the same register layout, so we can
            // consolidate into a single branch ere.
            PortDev::Dev1g | PortDev::Dev2g5 => {
                let dev = match cfg.dev.0 {
                    PortDev::Dev1g => DevGeneric::new_1g(cfg.dev.1),
                    PortDev::Dev2g5 => DevGeneric::new_2g5(cfg.dev.1),
                    _ => unreachable!(),
                }
                .map_err(MonorailError::from)?;
                let reg = self
                    .vsc7448
                    .read(dev.regs().PCS1G_CFG_STATUS().PCS1G_LINK_STATUS())
                    .map_err(MonorailError::from)?;
                (reg.link_status() != 0) && (reg.signal_detect() != 0)
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

    fn get_port_counters(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
    ) -> Result<PortCounters, RequestError<MonorailError>> {
        if usize::from(port) >= self.map.len() {
            return Err(MonorailError::InvalidPort.into());
        }
        let cfg = match self.map.port_config(port) {
            None => return Err(MonorailError::UnconfiguredPort.into()),
            Some(cfg) => cfg,
        };
        let (tx, rx) = match cfg.dev.0 {
            PortDev::Dev1g | PortDev::Dev2g5 => {
                let stats = ASM().DEV_STATISTICS(port);
                let rx_uc = self
                    .vsc7448
                    .read(stats.RX_UC_CNT())
                    .map_err(MonorailError::from)?;
                let rx_bc = self
                    .vsc7448
                    .read(stats.RX_BC_CNT())
                    .map_err(MonorailError::from)?;
                let rx_mc = self
                    .vsc7448
                    .read(stats.RX_MC_CNT())
                    .map_err(MonorailError::from)?;
                let tx_uc = self
                    .vsc7448
                    .read(stats.TX_UC_CNT())
                    .map_err(MonorailError::from)?;
                let tx_bc = self
                    .vsc7448
                    .read(stats.TX_BC_CNT())
                    .map_err(MonorailError::from)?;
                let tx_mc = self
                    .vsc7448
                    .read(stats.TX_MC_CNT())
                    .map_err(MonorailError::from)?;
                let tx = PacketCount {
                    unicast: tx_uc.into(),
                    multicast: tx_mc.into(),
                    broadcast: tx_bc.into(),
                };
                let rx = PacketCount {
                    unicast: rx_uc.into(),
                    multicast: rx_mc.into(),
                    broadcast: rx_bc.into(),
                };
                (tx, rx)
            }
            PortDev::Dev10g => {
                let stats = DEV10G(cfg.dev.1).DEV_STATISTICS_32BIT();
                let rx_uc = self
                    .vsc7448
                    .read(stats.RX_UC_CNT())
                    .map_err(MonorailError::from)?;
                let rx_bc = self
                    .vsc7448
                    .read(stats.RX_BC_CNT())
                    .map_err(MonorailError::from)?;
                let rx_mc = self
                    .vsc7448
                    .read(stats.RX_MC_CNT())
                    .map_err(MonorailError::from)?;
                let tx_uc = self
                    .vsc7448
                    .read(stats.TX_UC_CNT())
                    .map_err(MonorailError::from)?;
                let tx_bc = self
                    .vsc7448
                    .read(stats.TX_BC_CNT())
                    .map_err(MonorailError::from)?;
                let tx_mc = self
                    .vsc7448
                    .read(stats.TX_MC_CNT())
                    .map_err(MonorailError::from)?;
                let tx = PacketCount {
                    unicast: tx_uc.into(),
                    multicast: tx_mc.into(),
                    broadcast: tx_bc.into(),
                };
                let rx = PacketCount {
                    unicast: rx_uc.into(),
                    multicast: rx_mc.into(),
                    broadcast: rx_bc.into(),
                };
                (tx, rx)
            }
        };
        Ok(PortCounters { tx, rx })
    }

    fn reset_port_counters(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
    ) -> Result<(), RequestError<MonorailError>> {
        if usize::from(port) >= self.map.len() {
            return Err(MonorailError::InvalidPort.into());
        }
        let cfg = match self.map.port_config(port) {
            None => return Err(MonorailError::UnconfiguredPort.into()),
            Some(cfg) => cfg,
        };
        match cfg.dev.0 {
            PortDev::Dev1g | PortDev::Dev2g5 => {
                let stats = ASM().DEV_STATISTICS(port);
                self.vsc7448
                    .write(stats.RX_UC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.RX_BC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.RX_MC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.TX_UC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.TX_BC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.TX_MC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
            }
            PortDev::Dev10g => {
                let stats = DEV10G(cfg.dev.1).DEV_STATISTICS_32BIT();
                self.vsc7448
                    .write(stats.RX_UC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.RX_BC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.RX_MC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.TX_UC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.TX_BC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
                self.vsc7448
                    .write(stats.TX_MC_CNT(), 0.into())
                    .map_err(MonorailError::from)?;
            }
        }
        Ok(())
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
            None => Err(MonorailError::NoPhy.into()),
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
        self.check_port(port)?;
        let addr = PhyRegisterAddress::from_page_and_addr_unchecked(page, reg);
        match self.bsp.phy_fn(port, |phy| phy.write(addr, value)) {
            None => Err(MonorailError::NoPhy.into()),
            Some(r) => {
                r.map_err(MonorailError::from).map_err(RequestError::from)
            }
        }
    }

    fn get_phy_status(
        &mut self,
        _msg: &userlib::RecvMessage,
        port: u8,
    ) -> Result<PhyStatus, RequestError<MonorailError>> {
        self.check_port(port)?;
        match self.bsp.phy_fn(port, |phy| {
            let ty = match phy.read_id()? {
                vsc85xx::vsc85x2::VSC8552_ID => PhyType::Vsc8552,
                vsc85xx::vsc85x2::VSC8562_ID => {
                    // See discussion in vsc85x2.rs
                    let rev = phy.read(phy::GPIO::EXTENDED_REVISION())?;
                    if u16::from(rev) & 0x4000 == 0 {
                        PhyType::Vsc8562
                    } else {
                        return Err(MonorailError::UnknownPhyId);
                    }
                }
                vsc85xx::vsc8504::VSC8504_ID => PhyType::Vsc8504,
                vsc85xx::vsc8522::VSC8522_ID => PhyType::Vsc8522,
                _ => return Err(MonorailError::UnknownPhyId),
            };
            let status = phy.read(phy::STANDARD::MODE_STATUS())?;
            let media_link_up = (status.0 & (1 << 2)) != 0;
            let status = phy.read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS())?;
            let mac_link_up = status.mac_link_status() != 0;
            Ok(PhyStatus {
                ty,
                mac_link_up,
                media_link_up,
            })
        }) {
            None => Err(MonorailError::NoPhy.into()),
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
    use super::{MonorailError, PhyStatus, PortCounters, PortStatus};
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
