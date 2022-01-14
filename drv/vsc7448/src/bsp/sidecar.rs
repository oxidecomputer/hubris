use crate::{
    dev::{dev10g_init_sfi, dev1g_init_sgmii, dev2g5_init_sgmii},
    phy::PhyRw,
    serdes10g, serdes1g, serdes6g,
    spi::Vsc7448Spi,
    VscError,
};
use drv_stm32h7_gpio_api as gpio_api;
use userlib::{hl::sleep_for, task_slot};
use vsc7448_pac::{types::PhyRegisterAddress, Vsc7448};

task_slot!(GPIO, gpio_driver);

pub struct Bsp<'a> {
    vsc7448: &'a Vsc7448Spi,
}

impl<'a> PhyRw for Bsp<'a> {
    fn read_raw<T: From<u16>>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        unimplemented!();
    }

    fn write_raw<T>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone,
    {
        unimplemented!();
    }
}

impl<'a> Bsp<'a> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448Spi) -> Result<Self, VscError> {
        let out = Bsp { vsc7448 };
        out.init()?;
        Ok(out)
    }

    pub fn init(&self) -> Result<(), VscError> {
        // See RFD144 for a detailed look at the design
        let gpio_driver = gpio_api::Gpio::from(GPIO.get_task_id());

        // Cubbies 0 through 7
        let serdes1g_cfg_sgmii = serdes1g::Config::new(serdes1g::Mode::Sgmii);
        for dev in 0..=7 {
            dev1g_init_sgmii(dev, &self.vsc7448)?;
            serdes1g_cfg_sgmii.apply(dev + 1, &self.vsc7448)?;
            // DEV1G[dev], SERDES1G[dev + 1], S[port + 1], SGMII
        }
        // Cubbies 8 through 21
        let serdes6g_cfg_sgmii = serdes6g::Config::new(serdes6g::Mode::Sgmii);
        for dev in 0..=13 {
            dev2g5_init_sgmii(dev, &self.vsc7448)?;
            serdes6g_cfg_sgmii.apply(dev, &self.vsc7448)?;
            // DEV2G5[dev], SERDES6G[dev], S[port + 1], SGMII
        }
        // Cubbies 22 through 29
        for dev in 16..=23 {
            dev2g5_init_sgmii(dev, &self.vsc7448)?;
            serdes6g_cfg_sgmii.apply(dev, &self.vsc7448)?;
            // DEV2G5[dev], SERDES6G[dev], S[port + 1], SGMII
        }
        // Cubbies 30 and 31
        let serdes10g_cfg_sgmii =
            serdes10g::Config::new(serdes10g::Mode::Sgmii)?;
        for dev in [27, 28] {
            dev2g5_init_sgmii(dev, &self.vsc7448)?;
            serdes10g_cfg_sgmii.apply(dev - 25, &self.vsc7448)?;
            // DEV2G5[dev], SERDES10G[dev - 25], S[dev + 8], SGMII
        }

        ////////////////////////////////////////////////////////////////////////
        // PSC0/1, Technician 0/1, a few unused ports
        // These go over 2x QSGMII links:
        // - Ports 16-19 go through SERDES6G_14 to an on-board VSC8504 PHY
        //   (PHY4, U40), which is configured over MIIM from the SP
        // - Ports 20-23 go through SERDES6G_15 to the front panel board

        // Let's configure the on-board PHY first
        // Relevant pins are
        // - MIIM_SP_TO_PHY_MDC_2V5
        // - MIIM_SP_TO_PHY_MDIO_2V5
        // - MIIM_SP_TO_PHY_MDINT_2V5_L
        // - SP_TO_PHY4_COMA_MODE (PI10, internal pull-up)
        // - SP_TO_PHY4_RESET_L (PI9)
        //
        // The PHY talks on MIIM addresses 0x4-0x7 (configured by resistors
        // on the board)

        // The PHY must be powered and RefClk must be up at this point
        //
        // Jiggle reset line, then wait 120 ms
        let coma_mode = gpio_api::Port::I.pin(10);
        gpio_driver.set(coma_mode).unwrap();
        gpio_driver
            .configure_output(
                coma_mode,
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::Low,
                gpio_api::Pull::None,
            )
            .unwrap();

        // Make NRST low then switch it to output mode
        let nrst = gpio_api::Port::I.pin(9);
        gpio_driver.reset(nrst).unwrap();
        gpio_driver
            .configure_output(
                nrst,
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::Low,
                gpio_api::Pull::None,
            )
            .unwrap();
        sleep_for(10);
        gpio_driver.set(nrst).unwrap();
        sleep_for(120); // Wait for the chip to come out of reset

        // Initialize the PHY, then disable COMA_MODE
        crate::phy::init_vsc8504_phy(0, self)?;
        gpio_driver.reset(coma_mode).unwrap();

        // Now that the PHY is configured, we can bring up the VSC7448.  This
        // is very similar to how we bring up QSGMII in the dev kit BSP
        // (bsp/gemini_bu.rs)
        self.vsc7448
            .modify(Vsc7448::HSIO().HW_CFGSTAT().HW_CFG(), |r| {
                // Enable QSGMII mode for DEV1G_16-23 via SerDes6G_14/15
                let ena = r.qsgmii_ena();
                r.set_qsgmii_ena(ena | (1 << 10) | (1 << 11));
            })?;
        for dev in 16..=23 {
            // Reset the PCS TX clock domain.  In the SDK, this is accompanied
            // by the cryptic comment "BZ23738", which may refer to an errata
            // of some kind?
            self.vsc7448.modify(
                Vsc7448::DEV1G(dev).DEV_CFG_STATUS().DEV_RST_CTRL(),
                |r| {
                    r.set_pcs_tx_rst(0);
                },
            )?;
        }
        let serdes6g_cfg_qsgmii = serdes6g::Config::new(serdes6g::Mode::Qsgmii);
        serdes6g_cfg_qsgmii.apply(14, &self.vsc7448)?;
        serdes6g_cfg_qsgmii.apply(15, &self.vsc7448)?;
        for dev in 16..=23 {
            dev1g_init_sgmii(dev, &self.vsc7448)?;
        }

        ////////////////////////////////////////////////////////////////////////
        // DEV2G5[24], SERDES1G[0], S0, SGMII to Local SP
        serdes1g_cfg_sgmii.apply(0, &self.vsc7448)?;
        dev1g_init_sgmii(24, &self.vsc7448)?;

        ////////////////////////////////////////////////////////////////////////
        // DEV10G[0], SERDES10G[0], S33, SFI to Tofino 2
        let serdes_cfg = serdes10g::Config::new(serdes10g::Mode::Lan10g)?;
        dev10g_init_sfi(0, &serdes_cfg, &self.vsc7448)?;

        Ok(())
    }

    pub fn run(&self) -> ! {
        loop {
            sleep_for(100);
        }
    }
}
