// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_user_leds_api::UserLeds;
use ringbuf::*;
use userlib::*;
use vsc7448::{
    dev::{Dev10g, DevGeneric, Speed},
    port, serdes10g, serdes6g,
    spi::Vsc7448Spi,
    VscError,
};
use vsc7448_pac::{phy, types::PhyRegisterAddress, Vsc7448};
use vsc85xx::{init_vsc8522_phy, Phy, PhyRw, PhyVsc85xx};

task_slot!(USER_LEDS, user_leds);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    PhyScanError { miim: u8, phy: u8, err: VscError },
    PhyLinkChanged { port: u8, status: u16 },
    SgmiiError { dev: u8, err: VscError },
    MacAddress([u8; 6]),
    VscErr(VscError),
}
ringbuf!(Trace, 16, Trace::None);

pub struct Bsp<'a> {
    vsc7448: &'a Vsc7448Spi,
    leds: UserLeds,
    phy_link_up: [[bool; 24]; 2],
    known_macs: [Option<[u8; 6]>; 16],
}

impl<'a> Bsp<'a> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448Spi) -> Result<Self, VscError> {
        let leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());
        let out = Bsp {
            vsc7448,
            leds,
            phy_link_up: Default::default(),
            known_macs: Default::default(),
        };
        out.init()?;
        Ok(out)
    }

    /// Initializes four ports on front panel RJ45 connectors
    fn init_rj45(&self) -> Result<(), VscError> {
        // The VSC7448 dev kit has 2x VSC8522 PHYs on each of MIIM1 and MIIM2.
        // Each PHYs on the same MIIM bus is strapped to different ports.
        for miim in [1, 2] {
            self.vsc7448
                .modify(Vsc7448::DEVCPU_GCB().MIIM(miim).MII_CFG(), |cfg| {
                    cfg.set_miim_cfg_prescale(0xFF)
                })?;
            // We only need to check this on one PHY port per physical PHY
            // chip.  Port 0 maps to one PHY chip, and port 12 maps to the
            // other one (controlled by hardware pull-ups).
            let mut phy_rw = Vsc7448SpiPhy::new(self.vsc7448, miim);
            for port in [0, 12] {
                let mut p = Phy {
                    port,
                    rw: &mut phy_rw,
                };
                init_vsc8522_phy(&mut p)?;
            }
        }

        // I want to configure ports 0-3 (or 1-4, depending on numbering) on
        // the VSC7448 to use QSGMII to talk on SERDES6G_4 to the VSC8522.
        //
        // The following code is based on port_setup in the MESA SDK, but
        // extracted and trimmed down to the bare necessacities (e.g. assuming
        // the chip is configured from reset)
        self.vsc7448
            .modify(Vsc7448::HSIO().HW_CFGSTAT().HW_CFG(), |r| {
                // Enable QSGMII mode for devices DEV1G_0, DEV1G_1, DEV1G_2, and
                // DEV1G_3 via SerDes6G_4.
                let ena = r.qsgmii_ena() | 1;
                r.set_qsgmii_ena(ena);
            })?;
        for port in 0..4 {
            // Reset the PCS TX clock domain.  In the SDK, this is accompanied
            // by the cryptic comment "BZ23738", which may refer to an errata
            // of some kind?
            self.vsc7448.modify(
                Vsc7448::DEV1G(port).DEV_CFG_STATUS().DEV_RST_CTRL(),
                |r| {
                    r.set_pcs_tx_rst(0);
                },
            )?;
        }

        // Configure SERDES6G_4 for QSGMII
        serdes6g::Config::new(serdes6g::Mode::Qsgmii)
            .apply(4, &self.vsc7448)?;

        for port in 0..4 {
            DevGeneric::new_1g(port)?
                .init_sgmii(&self.vsc7448, Speed::Speed1G)?;
        }
        Ok(())
    }

    /// Initializes two ports on front panel SFP+ connectors
    fn init_sfp(&self) -> Result<(), VscError> {
        //  Now, let's bring up two SFP+ ports
        //
        //  SFP ports A and B are connected to S33/34 using SFI.  We need to
        //  bring up 10G SERDES then enable the ports

        // HW_CFG is already set up for 10G on all four DEV10G

        let serdes_cfg = serdes10g::Config::new(serdes10g::Mode::Lan10g)?;
        for dev in [0, 1] {
            let dev = Dev10g::new(dev)?;
            dev.init_sfi(&self.vsc7448)?;
            serdes_cfg.apply(dev.index(), &self.vsc7448)?;
        }

        Ok(())
    }

    /// Configures a port to run DEV2G5_XX through SERDES10G_3 through S36.
    /// This isn't actually valid for the dev kit, which expects SFI, but as
    /// long as you don't plug anything into that port, it's _fine_.
    ///
    /// `d2g5` must be a valid DEV2G5; `d10g` must be the DEV10G that it shadows
    fn init_10g_sgmii(&self, d2g5: u8, d10g: u8) -> Result<(), VscError> {
        // We have to disable and flush the 10G port that shadows this port
        port::port10g_flush(&Dev10g::new(d10g).unwrap(), self.vsc7448)?;

        // "Configure the 10G Mux mode to DEV2G5"
        self.vsc7448
            .modify(Vsc7448::HSIO().HW_CFGSTAT().HW_CFG(), |r| match d10g {
                0 => r.set_dev10g_0_mode(3),
                1 => r.set_dev10g_1_mode(3),
                2 => r.set_dev10g_2_mode(3),
                3 => r.set_dev10g_3_mode(3),
                d => panic!("Invalid DEV10G {}", d),
            })?;

        let dev_2g5 = DevGeneric::new_2g5(d2g5)?;
        // This bit must be set when a 10G port runs below 10G speed
        self.vsc7448.modify(
            Vsc7448::DSM().CFG().DEV_TX_STOP_WM_CFG(dev_2g5.port()),
            |r| {
                r.set_dev10g_shadow_ena(1);
            },
        )?;
        let serdes10g_cfg_sgmii =
            serdes10g::Config::new(serdes10g::Mode::Sgmii)?;
        serdes10g_cfg_sgmii.apply(d10g, &self.vsc7448)?;
        dev_2g5.init_sgmii(&self.vsc7448, Speed::Speed100M)?;
        Ok(())
    }

    fn gpio_init(&self) -> Result<(), VscError> {
        // We assume that the only person running on a gemini-bu-1 is Matt, who is
        // talking to a VSC7448 dev kit on his desk.  In this case, we want to
        // configure the GPIOs to allow MIIM1 and 2 to be active, by setting
        // GPIO_56-59 to Overlaid Function 1
        self.vsc7448.write(
            Vsc7448::DEVCPU_GCB().GPIO().GPIO_ALT1(0),
            0xF000000.into(),
        )?;
        Ok(())
    }

    /// Attempts to initialize the system.  This is based on a VSC7448 dev kit
    /// (VSC5627EV), so will need to change depending on your system.
    fn init(&self) -> Result<(), VscError> {
        self.gpio_init()?;
        self.init_rj45()?;
        self.init_sfp()?;
        self.init_10g_sgmii(27, 2)?; // DEV2G5_27, SERDES10G_2, S35
        self.init_10g_sgmii(28, 3)?; // DEV2G5_28, SERDES10G_3, S36

        self.leds.led_off(0).unwrap();
        self.leds.led_on(3).unwrap();
        Ok(())
    }

    /// Checks the given PHY's status, return `true` if the link is up
    fn check_phy(&mut self, miim: u8, phy: u8) -> bool {
        let mut phy_rw = Vsc7448SpiPhy::new(self.vsc7448, miim);
        let mut p = Phy {
            port: phy,
            rw: &mut phy_rw,
        };
        match p.read(phy::STANDARD::MODE_STATUS()) {
            Ok(status) => {
                let up = (status.0 & (1 << 5)) != 0;
                if up != self.phy_link_up[miim as usize - 1][phy as usize] {
                    self.phy_link_up[miim as usize - 1][phy as usize] = up;
                    ringbuf_entry!(Trace::PhyLinkChanged {
                        port: (miim - 1) * 24 + phy,
                        status: status.0,
                    });
                }
                up
            }
            Err(err) => {
                ringbuf_entry!(Trace::PhyScanError { miim, phy, err });
                false
            }
        }
    }

    fn check_sgmii(&mut self, dev: u8) -> bool {
        match self
            .vsc7448
            .read(Vsc7448::DEV2G5(dev).PCS1G_CFG_STATUS().PCS1G_LINK_STATUS())
        {
            Ok(v) => v.link_status() != 0,
            Err(err) => {
                ringbuf_entry!(Trace::SgmiiError { dev, err });
                false
            }
        }
    }

    fn wake(&mut self) -> Result<(), VscError> {
        let mut any_phy_up = false;
        for miim in [1, 2] {
            for phy in 0..24 {
                any_phy_up |= self.check_phy(miim, phy);
            }
        }
        if any_phy_up {
            self.leds.led_on(1).unwrap();
        } else {
            self.leds.led_off(1).unwrap();
        }

        let mut any_sgmii_up = false;
        for d in [27, 28] {
            any_sgmii_up |= self.check_sgmii(d);
        }
        if any_sgmii_up {
            self.leds.led_on(2).unwrap();
        } else {
            self.leds.led_off(2).unwrap();
        }

        // Dump the MAC tables
        loop {
            // Trigger a FIND_SMALLEST action then wait for it to finish
            let ctrl = Vsc7448::LRN().COMMON().COMMON_ACCESS_CTRL();
            self.vsc7448.write_with(ctrl, |r| {
                r.set_cpu_access_cmd(0x6); // FIND_SMALLEST
                r.set_mac_table_access_shot(0x1); // run
            })?;
            while self.vsc7448.read(ctrl)?.mac_table_access_shot() == 1 {
                hl::sleep_for(1);
            }

            let msb = self
                .vsc7448
                .read(Vsc7448::LRN().COMMON().MAC_ACCESS_CFG_0())?
                .mac_entry_mac_msb();
            let lsb = self
                .vsc7448
                .read(Vsc7448::LRN().COMMON().MAC_ACCESS_CFG_1())?
                .mac_entry_mac_lsb();
            if msb == 0 && lsb == 0 {
                break;
            } else {
                let mut mac = [0; 6];
                mac[0..2].copy_from_slice(&msb.to_be_bytes()[2..]);
                mac[2..6].copy_from_slice(&lsb.to_be_bytes());

                // Inefficient but easy way to avoid logging MAC addresses
                // repeatedly.  This will fail to scale for larger systems,
                // where we'd want some kind of LRU cache, but is nice
                // for debugging.
                let mut mac_is_new = true;
                for m in self.known_macs.iter_mut() {
                    match m {
                        Some(m) => {
                            if *m == mac {
                                mac_is_new = false;
                                break;
                            }
                        }
                        None => {
                            *m = Some(mac);
                            break;
                        }
                    }
                }
                if mac_is_new {
                    ringbuf_entry!(Trace::MacAddress(mac));
                }
            }
        }
        Ok(())
    }

    pub fn run(&mut self) -> ! {
        loop {
            hl::sleep_for(500);
            if let Err(e) = self.wake() {
                ringbuf_entry!(Trace::VscErr(e));
            }
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct Vsc7448SpiPhy<'a> {
    vsc7448: &'a Vsc7448Spi,
    miim: u8,
}

impl<'a> Vsc7448SpiPhy<'a> {
    pub fn new(vsc7448: &'a Vsc7448Spi, miim: u8) -> Self {
        Self { vsc7448, miim }
    }
    /// Builds a MII_CMD register based on the given phy and register.  Note
    /// that miim_cmd_opr_field is unset; you must configure it for a read
    /// or write yourself.
    fn miim_cmd(
        phy: u8,
        reg_addr: u8,
    ) -> vsc7448_pac::devcpu_gcb::miim::MII_CMD {
        let mut v: vsc7448_pac::devcpu_gcb::miim::MII_CMD = 0.into();
        v.set_miim_cmd_vld(1);
        v.set_miim_cmd_phyad(phy as u32);
        v.set_miim_cmd_regad(reg_addr as u32);
        v
    }

    /// Waits for the PENDING_RD and PENDING_WR bits to go low, indicating that
    /// it's safe to read or write to the MIIM.
    fn miim_idle_wait(&self) -> Result<(), VscError> {
        for _i in 0..32 {
            let status = self
                .vsc7448
                .read(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_STATUS())?;
            if status.miim_stat_opr_pend() == 0 {
                return Ok(());
            }
        }
        return Err(VscError::MiimIdleTimeout);
    }

    /// Waits for the STAT_BUSY bit to go low, indicating that a read has
    /// finished and data is available.
    fn miim_read_wait(&self) -> Result<(), VscError> {
        for _i in 0..32 {
            let status = self
                .vsc7448
                .read(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_STATUS())?;
            if status.miim_stat_busy() == 0 {
                return Ok(());
            }
        }
        return Err(VscError::MiimReadTimeout);
    }
}

impl PhyRw for Vsc7448SpiPhy<'_> {
    fn read_raw<T: From<u16>>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        let mut v = Self::miim_cmd(phy, reg.addr);
        v.set_miim_cmd_opr_field(0b10); // read

        self.miim_idle_wait()?;
        self.vsc7448
            .write(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_CMD(), v)?;
        self.miim_read_wait()?;

        let out = self
            .vsc7448
            .read(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_DATA())?;
        if out.miim_data_success() == 0b11 {
            return Err(VscError::MiimReadErr {
                miim: self.miim,
                phy,
                page: reg.page,
                addr: reg.addr,
            });
        }

        let value = out.miim_data_rddata() as u16;
        Ok(value.into())
    }

    fn write_raw<T>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone,
    {
        let value: u16 = value.into();
        let mut v = Self::miim_cmd(phy, reg.addr);
        v.set_miim_cmd_opr_field(0b01); // read
        v.set_miim_cmd_wrdata(value as u32);

        self.miim_idle_wait()?;
        self.vsc7448
            .write(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_CMD(), v)
    }
}

/// Turns on LEDs to let the user know that the board is alive and starting
/// initialization (we'll turn these off at the end of Bsp::init)
pub fn preinit() {
    let leds = drv_user_leds_api::UserLeds::from(USER_LEDS.get_task_id());
    leds.led_off(1).unwrap();
    leds.led_off(2).unwrap();
    leds.led_off(3).unwrap();

    leds.led_on(0).unwrap();
}

// In this system, we're talking to a VSC8522, which is in the VSC85xx family
// and compatible with its control and config functions.
impl PhyVsc85xx for Vsc7448SpiPhy<'_> {}
