// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    bsp::{self, Bsp},
    notifications,
};
use drv_monorail_api::{
    LinkStatus, MacTableEntry, MonorailError, PacketCount, PhyStatus, PhyType,
    PortCounters, PortDev, PortStatus, VscError,
};
use idol_runtime::{NotificationHandler, RequestError};
use userlib::{sys_get_timer, sys_set_timer};
use vsc7448::{
    config::{PortMap, PortMode},
    DevGeneric, Vsc7448, Vsc7448Rw, PORT_COUNT,
};
use vsc7448_pac::{types::PhyRegisterAddress, *};

pub struct ServerImpl<'a, R> {
    bsp: Bsp<'a, R>,
    vsc7448: &'a Vsc7448<'a, R>,
    map: &'a PortMap,
    wake_target_time: u64,

    /// For monitoring purposes, we want a sticky bit that indicates whether a
    /// link has gone down from the perspective of an attached PHY.
    ///
    /// However, the PHY registers typically use self-clearing bits.  We cache
    /// the bit here, so that it can be explicitly cleared.
    phy_link_down_sticky: [bool; PORT_COUNT],
}

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

        // Trigger a wake IRQ right away
        sys_set_timer(Some(0), notifications::WAKE_TIMER_MASK);
        Self {
            bsp,
            wake_target_time,
            map,
            vsc7448,
            phy_link_down_sticky: [false; PORT_COUNT],
        }
    }

    pub fn wake(&mut self) -> Result<(), VscError> {
        let now = sys_get_timer().now;
        if let Some(wake_interval) = bsp::WAKE_INTERVAL {
            if now >= self.wake_target_time {
                let out = self.bsp.wake();
                self.wake_target_time = userlib::set_timer_relative(
                    wake_interval,
                    notifications::WAKE_TIMER_MASK,
                );
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

    fn decode_phy_id<P: vsc85xx::PhyRw>(
        phy: &vsc85xx::Phy<'_, P>,
    ) -> Result<(u32, PhyType), VscError> {
        let id = phy.read_id()?;
        let ty = match id {
            vsc85xx::vsc85x2::VSC8552_ID => PhyType::Vsc8552,
            vsc85xx::vsc85x2::VSC8562_ID => {
                // See discussion in vsc85x2.rs
                let rev = phy.read(phy::GPIO::EXTENDED_REVISION())?;
                if u16::from(rev) & 0x4000 == 0 {
                    PhyType::Vsc8562
                } else {
                    return Err(VscError::UnknownPhyId(id));
                }
            }
            vsc85xx::vsc8504::VSC8504_ID => PhyType::Vsc8504,
            vsc85xx::vsc8522::VSC8522_ID => PhyType::Vsc8522,
            _ => return Err(VscError::UnknownPhyId(id)),
        };
        Ok((id, ty))
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
        let mut link_up = match cfg.dev.0 {
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

                if reg.link_status() == 0 {
                    LinkStatus::Down
                } else if reg.signal_detect() == 0 || reg.sync_status() == 0 {
                    LinkStatus::Error
                } else {
                    LinkStatus::Up
                }
            }
            PortDev::Dev10g => {
                // Section of 3.8.2.2 describes how to monitor link status for
                // DEV10G, which isn't as simple as the DEV1G/2G5.
                if self
                    .vsc7448
                    .read(PCS10G_BR(cfg.dev.1).PCS_10GBR_STATUS().PCS_STATUS())
                    .map_err(MonorailError::from)?
                    .rx_block_lock()
                    != 0
                {
                    LinkStatus::Up
                } else {
                    LinkStatus::Down
                }
            }
        };
        // If this is a QSGMII port, also check the QSGMII status register
        if matches!(self.map[port], Some(PortMode::Qsgmii(_))) {
            let r = self
                .vsc7448
                .read(HSIO().HW_CFGSTAT().HW_QSGMII_STAT(port / 4))
                .map_err(MonorailError::from)?;
            if r.sync() == 0 && link_up == LinkStatus::Up {
                link_up = LinkStatus::Error;
            }
        }

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
        let (tx, rx, link_down_sticky, phy_link_down_sticky) = match cfg.dev.0 {
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

                // TODO: if this port uses a PHY, then should we be checking
                // the PHY's status instead of ours?
                let dev = match cfg.dev.0 {
                    PortDev::Dev1g => DevGeneric::new_1g(cfg.dev.1),
                    PortDev::Dev2g5 => DevGeneric::new_2g5(cfg.dev.1),
                    _ => unreachable!(),
                }
                .map_err(MonorailError::from)?;

                let link_down = self
                    .vsc7448
                    .read(dev.regs().PCS1G_CFG_STATUS().PCS1G_STICKY())
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
                let link_down_sticky = link_down.link_down_sticky() != 0
                    || link_down.out_of_sync_sticky() != 0;

                // Take the union of the "link changed" bit on the PHY and our
                // local sticky value (since the PHY bit is self-resetting)
                let r = self.bsp.phy_fn(port, |phy| {
                    phy.read(phy::STANDARD::INTERRUPT_STATUS())
                });
                let v = match r {
                    // If there is no PHY present, then the PHY link down
                    // indication is always false.
                    None => false,
                    // Otherwise, bit 13 is "Link state change mask"
                    Some(r) => r
                        .map(|r| r.0 & (1 << 13) != 0)
                        .map_err(MonorailError::from)?,
                };
                self.phy_link_down_sticky[port as usize] |= v;

                (
                    tx,
                    rx,
                    link_down_sticky,
                    self.phy_link_down_sticky[port as usize],
                )
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

                let intr = self
                    .vsc7448
                    .read(
                        PCS10G_BR(cfg.dev.1).PCS_10GBR_STATUS().PCS_INTR_STAT(),
                    )
                    .map_err(MonorailError::from)?;
                let link_down_sticky = intr.lock_changed_sticky() != 0;

                (tx, rx, link_down_sticky, false)
            }
        };
        Ok(PortCounters {
            tx,
            rx,
            link_down_sticky,
            phy_link_down_sticky,
        })
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

                let dev = match cfg.dev.0 {
                    PortDev::Dev1g => DevGeneric::new_1g(cfg.dev.1),
                    PortDev::Dev2g5 => DevGeneric::new_2g5(cfg.dev.1),
                    _ => unreachable!(),
                }
                .map_err(MonorailError::from)?;

                // Clear our local sticky bit, then read the PHY register (to
                // clear the self-clearing bit).  We don't care about the actual
                // register value here; just the side effect of reading it.
                self.phy_link_down_sticky[port as usize] = false;
                if let Some(Err(e)) = self.bsp.phy_fn(port, |phy| {
                    phy.read(phy::STANDARD::INTERRUPT_STATUS())
                }) {
                    return Err(RequestError::from(MonorailError::from(e)));
                }

                // Clear the two bits that we use to detect link drops
                self.vsc7448
                    .write_with(
                        dev.regs().PCS1G_CFG_STATUS().PCS1G_STICKY(),
                        |r| {
                            r.set_link_down_sticky(1);
                            r.set_out_of_sync_sticky(1);
                        },
                    )
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

                self.vsc7448
                    .write_with(
                        PCS10G_BR(cfg.dev.1).PCS_10GBR_STATUS().PCS_INTR_STAT(),
                        |r| r.set_lock_changed_sticky(1),
                    )
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
        let phy_result =
            self.bsp.phy_fn(port, |phy| -> Result<PhyStatus, VscError> {
                let (_id, ty) = Self::decode_phy_id(&phy)?;
                let status = phy.read(phy::STANDARD::MODE_STATUS())?;
                let media_link_up = (status.0 & (1 << 2)) != 0;

                // The VSC8504 is running in forced-speed protocol transfer mode.
                // Experimentally, packets get through without MAC_LINK_STATUS
                // set, and despite what "ENT-AN1175" says, I don't see anything
                // in register 24G.  As such, we'll be optimistic: if there's a
                // valid QSGMII link and MAC_PCS_SIG_DETECT, then let's call it
                // good.
                let status =
                    phy.read(phy::EXTENDED_3::MAC_SERDES_PCS_STATUS())?;
                let mac_serdes =
                    phy.read(phy::EXTENDED_3::MAC_SERDES_STATUS())?;
                let qsgmii_mask = ty.qsgmii_okay_mask();
                let mac_link_up = match ty {
                    PhyType::Vsc8504 => {
                        if status.mac_pcs_sig_detect() == 0 {
                            LinkStatus::Down
                        } else if status.mac_sync_fail() != 0
                            || status.mac_cgbad() != 0
                            || (mac_serdes.0 & qsgmii_mask) != qsgmii_mask
                        {
                            LinkStatus::Error
                        } else {
                            LinkStatus::Up
                        }
                    }
                    PhyType::Vsc8522 | PhyType::Vsc8552 | PhyType::Vsc8562 => {
                        if status.mac_link_status() == 0 {
                            LinkStatus::Down
                        } else if status.mac_sync_fail() != 0
                            || status.mac_cgbad() != 0
                            || status.mac_pcs_sig_detect() == 0
                            || (mac_serdes.0 & qsgmii_mask) != qsgmii_mask
                        {
                            LinkStatus::Error
                        } else {
                            LinkStatus::Up
                        }
                    }
                };

                Ok(PhyStatus {
                    ty,
                    mac_link_up,
                    media_link_up: if media_link_up {
                        LinkStatus::Up
                    } else {
                        LinkStatus::Down
                    },
                })
            });
        match phy_result {
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

    fn read_vsc8504_sd6g_patch(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<vsc85xx::tesla::TeslaSerdes6gPatch, RequestError<MonorailError>>
    {
        const VSC8504_BASE_PORT: u8 = 40;
        self.bsp
            .phy_fn(VSC8504_BASE_PORT, |mut phy| {
                let (id, ty) = Self::decode_phy_id(&phy)?;
                if ty == PhyType::Vsc8504 {
                    vsc85xx::tesla::TeslaPhy { phy: &mut phy }
                        .read_patch_settings()
                } else {
                    Err(VscError::BadPhyId(id))
                }
            })
            .unwrap()
            .map_err(MonorailError::from)
            .map_err(RequestError::from)
    }

    fn read_vsc8504_sd6g_ob_config(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<
        vsc85xx::tesla::TeslaSerdes6gObConfig,
        RequestError<MonorailError>,
    > {
        const VSC8504_BASE_PORT: u8 = 40;
        self.bsp
            .phy_fn(VSC8504_BASE_PORT, |mut phy| {
                let (id, ty) = Self::decode_phy_id(&phy)?;
                if ty == PhyType::Vsc8504 {
                    vsc85xx::tesla::TeslaPhy { phy: &mut phy }
                        .read_serdes6g_ob()
                } else {
                    Err(VscError::BadPhyId(id))
                }
            })
            .unwrap()
            .map_err(MonorailError::from)
            .map_err(RequestError::from)
    }

    /// Exposes internal details of the VSC8504's SERDES6G for tuning
    ///
    /// This can only be called on Sidecar proper, not the VSC7448 dev kit.
    fn write_vsc8504_sd6g_ob_config(
        &mut self,
        _msg: &userlib::RecvMessage,
        ob_post0: u8,
        ob_post1: u8,
        ob_prec: u8,
        ob_sr_h: bool,
        ob_sr: u8,
    ) -> Result<(), RequestError<MonorailError>> {
        const VSC8504_BASE_PORT: u8 = 40;
        self.bsp
            .phy_fn(VSC8504_BASE_PORT, |mut phy| {
                let (id, ty) = Self::decode_phy_id(&phy)?;
                if ty == PhyType::Vsc8504 {
                    let mut tesla = vsc85xx::tesla::TeslaPhy { phy: &mut phy };
                    tesla.tune_serdes6g_ob(
                        vsc85xx::tesla::TeslaSerdes6gObConfig {
                            ob_post0,
                            ob_post1,
                            ob_prec,
                            ob_sr_h: u8::from(ob_sr_h),
                            ob_sr,
                        },
                    )
                } else {
                    Err(VscError::BadPhyId(id))
                }
            })
            .unwrap()
            .map_err(MonorailError::from)
            .map_err(RequestError::from)
    }

    /// Exposes internal details of the VSC8562's SERDES6G for tuning
    ///
    /// This can only be called on Sidecar proper, not the VSC7448 dev kit.
    fn write_vsc8562_sd6g_ob_cfg(
        &mut self,
        _msg: &userlib::RecvMessage,
        ob_ena1v_mode: u8,
        ob_pol: u8,
        ob_post0: u8,
        ob_post1: u8,
        ob_sr_h: u8,
        ob_resistor_ctr: u8,
        ob_sr: u8,
    ) -> Result<(), RequestError<MonorailError>> {
        const VSC8564_BASE_PORT: u8 = 44;
        let r = self.bsp.phy_fn(VSC8564_BASE_PORT, |mut phy| {
            let (id, ty) = Self::decode_phy_id(&phy)?;
            if ty == PhyType::Vsc8562 {
                use vsc85xx::vsc8562::{Sd6gObCfg, Vsc8562Phy};
                let mut v = Vsc8562Phy { phy: &mut phy };
                v.tune_sd6g_ob_cfg(Sd6gObCfg {
                    ob_ena1v_mode,
                    ob_pol,
                    ob_post0,
                    ob_post1,
                    ob_sr_h,
                    ob_resistor_ctr,
                    ob_sr,
                })
            } else {
                Err(VscError::BadPhyId(id))
            }
        });
        match r {
            None => Err(MonorailError::NoPhy.into()),
            Some(r) => {
                r.map_err(MonorailError::from).map_err(RequestError::from)
            }
        }
    }

    /// Exposes internal details of the VSC8562's SERDES6G for tuning
    ///
    /// This can only be called on Sidecar proper, not the VSC7448 dev kit.
    fn write_vsc8562_sd6g_ob_cfg1(
        &mut self,
        _msg: &userlib::RecvMessage,
        ob_ena_cas: u8,
        ob_lev: u8,
    ) -> Result<(), RequestError<MonorailError>> {
        const VSC8564_BASE_PORT: u8 = 44;
        let r = self.bsp.phy_fn(VSC8564_BASE_PORT, |mut phy| {
            let (id, ty) = Self::decode_phy_id(&phy)?;
            if ty == PhyType::Vsc8562 {
                use vsc85xx::vsc8562::{Sd6gObCfg1, Vsc8562Phy};
                let mut v = Vsc8562Phy { phy: &mut phy };
                v.tune_sd6g_ob_cfg1(Sd6gObCfg1 { ob_ena_cas, ob_lev })
            } else {
                Err(VscError::BadPhyId(id))
            }
        });
        match r {
            None => Err(MonorailError::NoPhy.into()),
            Some(r) => {
                r.map_err(MonorailError::from).map_err(RequestError::from)
            }
        }
    }

    /// Exposes internal details of the VSC8562's SERDES6G for tuning
    ///
    /// This can only be called on Sidecar proper, not the VSC7448 dev kit.
    fn read_vsc8562_sd6g_ob_cfg1(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<vsc85xx::vsc8562::Sd6gObCfg1, RequestError<MonorailError>> {
        const VSC8564_BASE_PORT: u8 = 44;
        let r = self.bsp.phy_fn(VSC8564_BASE_PORT, |mut phy| {
            let (id, ty) = Self::decode_phy_id(&phy)?;
            if ty == PhyType::Vsc8562 {
                let mut v = vsc85xx::vsc8562::Vsc8562Phy { phy: &mut phy };
                v.read_sd6g_ob_cfg1()
            } else {
                Err(VscError::BadPhyId(id))
            }
        });
        match r {
            None => Err(MonorailError::NoPhy.into()),
            Some(r) => {
                r.map_err(MonorailError::from).map_err(RequestError::from)
            }
        }
    }

    /// Exposes internal details of the VSC8562's SERDES6G for tuning
    ///
    /// This can only be called on Sidecar proper, not the VSC7448 dev kit.
    fn read_vsc8562_sd6g_ob_cfg(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<vsc85xx::vsc8562::Sd6gObCfg, RequestError<MonorailError>> {
        const VSC8564_BASE_PORT: u8 = 44;
        let r = self.bsp.phy_fn(VSC8564_BASE_PORT, |mut phy| {
            let (id, ty) = Self::decode_phy_id(&phy)?;
            if ty == PhyType::Vsc8562 {
                let mut v = vsc85xx::vsc8562::Vsc8562Phy { phy: &mut phy };
                v.read_sd6g_ob_cfg()
            } else {
                Err(VscError::BadPhyId(id))
            }
        });
        match r {
            None => Err(MonorailError::NoPhy.into()),
            Some(r) => {
                r.map_err(MonorailError::from).map_err(RequestError::from)
            }
        }
    }

    fn read_vsc7448_mac_count(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<usize, RequestError<MonorailError>> {
        vsc7448::mac::count_macs(self.vsc7448.rw)
            .map_err(MonorailError::from)
            .map_err(RequestError::from)
    }

    fn read_vsc7448_next_mac(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<MacTableEntry, RequestError<MonorailError>> {
        let mac = vsc7448::mac::next_mac(self.vsc7448.rw)
            .map_err(MonorailError::from)
            .map_err(RequestError::from)?;
        // Handle the case where there is no next MAC, e.g. because of a TOCTOU
        // race with `read_vsc7448_mac_count`
        //
        // TODO: switch to an `Option<MacTableEntry>` once we stop using
        // `ssmarshal` for Idol operations
        let out = match mac {
            Some(mac) => MacTableEntry {
                port: mac.addr,
                mac: mac.mac,
            },
            None => MacTableEntry {
                port: u16::MAX,
                mac: [0; 6],
            },
        };
        Ok(out)
    }

    fn reinit(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError<MonorailError>> {
        self.bsp
            .reinit()
            .map_err(MonorailError::from)
            .map_err(RequestError::from)
    }

    fn unlock_vlans(
        &mut self,
        _mgs: &userlib::RecvMessage,
        unlock_until: u64,
    ) -> Result<(), RequestError<MonorailError>> {
        self.bsp.unlock_vlans_until(unlock_until)
    }

    fn lock_vlans(
        &mut self,
        _mgs: &userlib::RecvMessage,
    ) -> Result<(), RequestError<MonorailError>> {
        self.bsp.lock_vlans()
    }
}

impl<'a, R> NotificationHandler for ServerImpl<'a, R> {
    fn current_notification_mask(&self) -> u32 {
        // We're always listening for the wake (timer) irq
        notifications::WAKE_TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        // Nothing to do here: the wake IRQ is handled in the main `net` loop
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
