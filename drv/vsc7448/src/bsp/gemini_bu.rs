use crate::{
    dev::{dev10g_init_sfi, dev1g_init_sgmii},
    phy::{init_miim_phy, PhyRw},
    serdes10g, serdes6g,
    spi::Vsc7448Spi,
    spi_phy::Vsc7448SpiPhy,
    VscError,
};
use ringbuf::*;
use userlib::*;
use vsc7448_pac::{phy, Vsc7448};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Initialized(u64),
    FailedToInitialize(VscError),
    PhyScanError { miim: u32, phy: u8, err: VscError },
    PhyLinkChanged { port: u32, status: u16 },
}
ringbuf!(Trace, 16, Trace::None);

pub struct Bsp<'a> {
    vsc7448: &'a Vsc7448Spi,
}
impl<'a> Bsp<'a> {
    /// Constructs and initializes a new BSP handle
    pub fn new(vsc7448: &'a Vsc7448Spi) -> Result<Self, VscError> {
        let out = Bsp { vsc7448 };
        out.init()?;
        Ok(out)
    }

    /// Attempts to initialize the system.  This is based on a VSC7448 dev kit
    /// (VSC5627EV), so will need to change depending on your system.
    fn init(&self) -> Result<(), VscError> {
        // We call into an inner function so that we can easily match on
        // errors here and log in the ringbuf.
        let out = self.init_inner();
        match out {
            Err(e) => ringbuf_entry!(Trace::FailedToInitialize(e)),
            Ok(_) => ringbuf_entry!(Trace::Initialized(sys_get_timer().now)),
        }
        out
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
            let p = Vsc7448SpiPhy::new(self.vsc7448, miim);
            init_miim_phy(&[0, 12], p)?;
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
            dev1g_init_sgmii(port, &self.vsc7448)?;
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
            dev10g_init_sfi(dev, &serdes_cfg, &self.vsc7448)?;
        }

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

        //  Bring up SFI I2C bus, so we can read from SFI EEPROMs (we don't
        //  actually use this for anything yet).
        //  I2C_SDA = GPIO15_TWI_SDA on the VSC7448 (alt "01")
        self.vsc7448.write(
            Vsc7448::DEVCPU_GCB().GPIO().GPIO_ALT(0),
            0x00008000.into(),
        )?;

        //  I2C_SCL = GPIO17_SI_nCS3 (for port A)
        //            GPIO18_SI_nCS3 (for port B)
        //            (both alt "10")
        self.vsc7448.write(
            Vsc7448::DEVCPU_GCB().GPIO().GPIO_ALT(1),
            0x00060000.into(),
        )?;
        Ok(())
    }

    fn init_inner(&self) -> Result<(), VscError> {
        self.gpio_init()?;
        self.init_rj45()?;
        self.init_sfp()?;

        Ok(())
    }

    pub fn run(&self) -> ! {
        let mut link_up = [[false; 24]; 2];
        loop {
            hl::sleep_for(100);
            for miim in [1, 2] {
                let p = Vsc7448SpiPhy::new(self.vsc7448, miim);
                for phy in 0..24 {
                    match p.read(phy, phy::STANDARD::MODE_STATUS()) {
                        Ok(status) => {
                            let up = (status.0 & (1 << 5)) != 0;
                            if up != link_up[miim as usize - 1][phy as usize] {
                                link_up[miim as usize - 1][phy as usize] = up;
                                ringbuf_entry!(Trace::PhyLinkChanged {
                                    port: (miim - 1) * 24 + phy as u32,
                                    status: status.0,
                                });
                            }
                        }
                        Err(err) => {
                            ringbuf_entry!(Trace::PhyScanError {
                                miim,
                                phy,
                                err
                            })
                        }
                    }
                }
            }
        }
    }
}
