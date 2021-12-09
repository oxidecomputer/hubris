#![no_std]
#![no_main]

// NOTE: you will probably want to remove this when you write your actual code;
// we need to import userlib to get this to compile, but it throws a warning
// because we're not actually using it yet!
use ringbuf::*;
#[allow(unused_imports)]
use userlib::*;

use drv_spi_api::{Spi, SpiDevice, SpiError};
use vsc7448_pac::{
    phy,
    types::{PhyRegisterAddress, RegisterAddress},
    Vsc7448,
};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Start(u64),
    Read {
        addr: u32,
        value: u32,
    },
    Write {
        addr: u32,
        value: u32,
    },
    MiimSetPage {
        miim: u8,
        phy: u8,
        page: u16,
    },
    MiimRead {
        miim: u8,
        phy: u8,
        page: u16,
        addr: u8,
        value: u16,
    },
    MiimWrite {
        miim: u8,
        phy: u8,
        page: u16,
        addr: u8,
        value: u16,
    },
    Initialized(u64),
    FailedToInitialize(VscError),
}

ringbuf!(Trace, 64, Trace::None);

task_slot!(SPI, spi_driver);
const VSC7448_SPI_DEVICE: u8 = 0;

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq)]
enum VscError {
    SpiError(SpiError),
    BadChipId(u32),
    BadMiimRead {
        miim: u8,
        phy: u8,
        page: u16,
        addr: u8,
    },
    BadPhyId1(u16),
    BadPhyId2(u16),
}

impl From<SpiError> for VscError {
    fn from(s: SpiError) -> Self {
        Self::SpiError(s)
    }
}

/// Helper struct to read and write from the VSC7448 over SPI
struct Vsc7448Spi(SpiDevice);
impl Vsc7448Spi {
    /// Reads from a VSC7448 register
    fn read<T>(&self, reg: RegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u32>,
    {
        assert!(reg.addr >= 0x71000000);
        assert!(reg.addr <= 0x72000000);
        let addr = (reg.addr & 0x00FFFFFF) >> 2;
        let data: [u8; 3] = [
            ((addr >> 16) & 0xFF) as u8,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
        ];

        // We read back 8 bytes in total:
        // - 3 bytes of address
        // - 1 byte of padding
        // - 4 bytes of data
        let mut out = [0; 8];
        self.0.exchange(&data[..], &mut out[..])?;
        let value = (out[7] as u32)
            | ((out[6] as u32) << 8)
            | ((out[5] as u32) << 16)
            | ((out[4] as u32) << 24);

        ringbuf_entry!(Trace::Read {
            addr: reg.addr,
            value
        });
        Ok(value.into())
    }

    /// Writes to a VSC7448 register.  This will overwrite the entire register;
    /// if you want to modify it, then use [Self::modify] instead.
    fn write<T>(
        &self,
        reg: RegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u32: From<T>,
    {
        assert!(reg.addr >= 0x71000000);
        assert!(reg.addr <= 0x72000000);

        let addr = (reg.addr & 0x00FFFFFF) >> 2;
        let value: u32 = value.into();
        let data: [u8; 7] = [
            0x80 | ((addr >> 16) & 0xFF) as u8,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
            ((value >> 24) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            (value & 0xFF) as u8,
        ];

        ringbuf_entry!(Trace::Write {
            addr: reg.addr,
            value: value.into()
        });
        self.0.write(&data[..])?;
        Ok(())
    }

    /// Performs a read-modify-write operation on a VSC7448 register
    fn modify<T, F>(
        &self,
        reg: RegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u32>,
        u32: From<T>,
        F: Fn(&mut T),
    {
        let mut data = self.read(reg)?;
        f(&mut data);
        self.write(reg, data)
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

    /// Writes a register to the PHY without modifying the page.  This
    /// shouldn't be called directly, as the page could be in an unknown
    /// state.
    fn phy_write_inner<T: From<u16>>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
    {
        let value: u16 = value.into();
        let mut v = Self::miim_cmd(phy, reg.addr);
        v.set_miim_cmd_opr_field(0b01); // read
        v.set_miim_cmd_wrdata(value as u32);

        self.write(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_CMD(), v)
    }

    /// Reads a register from the PHY without modifying the page.  This
    /// shouldn't be called directly, as the page could be in an unknown
    /// state.
    fn phy_read_inner<T: From<u16>>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        let mut v = Self::miim_cmd(phy, reg.addr);
        v.set_miim_cmd_opr_field(0b10); // read

        self.write(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_CMD(), v)?;
        let out =
            self.read(Vsc7448::DEVCPU_GCB().MIIM(miim as u32).MII_DATA())?;
        if out.miim_data_success() == 0b11 {
            return Err(VscError::BadMiimRead {
                miim,
                phy,
                page: reg.page,
                addr: reg.addr,
            });
        }

        let value = out.miim_data_rddata() as u16;
        Ok(value.into())
    }

    /// Reads a register from the PHY
    fn phy_read<T>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        ringbuf_entry!(Trace::MiimSetPage {
            miim,
            phy,
            page: reg.page,
        });
        self.phy_write_inner::<phy::standard::PAGE>(
            miim,
            phy,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        let out = self.phy_read_inner(miim, phy, reg)?;
        ringbuf_entry!(Trace::MiimRead {
            miim,
            phy,
            page: reg.page,
            addr: reg.addr,
            value: out.clone().into(),
        });
        Ok(out)
    }

    /// Writes a register to the PHY
    fn phy_write<T>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        ringbuf_entry!(Trace::MiimSetPage {
            miim,
            phy,
            page: reg.page,
        });
        self.phy_write_inner::<phy::standard::PAGE>(
            miim,
            phy,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        ringbuf_entry!(Trace::MiimWrite {
            miim,
            phy,
            page: reg.page,
            addr: reg.addr,
            value: value.clone().into(),
        });
        self.phy_write_inner(miim, phy, reg, value)
    }

    /// Performs a read-modify-write operation on a VSC7448 register
    fn phy_modify<T, F>(
        &self,
        miim: u8,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(&mut T),
    {
        let mut data = self.phy_read(miim, phy, reg)?;
        f(&mut data);
        self.phy_write(miim, phy, reg, data)
    }
}

////////////////////////////////////////////////////////////////////////////////
#[cfg(target_board = "gemini-bu-1")]
fn bsp_init(vsc7448: &Vsc7448Spi) -> Result<(), VscError> {
    // We assume that the only person running on a gemini-bu-1 is Matt, who is
    // talking to a VSC7448 dev kit on his desk.  In this case, we want to
    // configure the GPIOs to allow MIIM1 and 2 to be active.
    vsc7448
        .write(Vsc7448::DEVCPU_GCB().GPIO().GPIO_ALT1(0), 0x3000000.into())?;

    // The VSC7448 dev kit has a VSC8522 PHY on MIIM1 and MIIM2
    let id1 = vsc7448.phy_read(1, 0, phy::STANDARD::IDENTIFIER_1())?.0;
    if id1 != 0x7 {
        return Err(VscError::BadPhyId1(id1));
    }
    let id2 = vsc7448.phy_read(1, 0, phy::STANDARD::IDENTIFIER_2())?.0;
    if id2 != 0x6f3 {
        return Err(VscError::BadPhyId2(id2));
    }

    // Disable COMA MODE, which keeps the chip holding itself in reset
    vsc7448.phy_modify(1, 0, phy::GPIO::GPIO_CONTROL_2(), |g| {
        g.set_coma_mode_output_enable(0)
    })?;
    Ok(())
}

fn init(vsc7448: &Vsc7448Spi) -> Result<(), VscError> {
    // Write the byte ordering / endianness configuration
    vsc7448.write(
        Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(),
        0x81818181.into(),
    )?;
    // Configure reads to include 1 padding byte, since we're reading quickly
    vsc7448.write(Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CFGSTAT(), 1.into())?;

    bsp_init(vsc7448)?;

    let chip_id = vsc7448.read(Vsc7448::DEVCPU_GCB().CHIP_REGS().CHIP_ID())?;
    if chip_id.rev_id() != 0x3
        || chip_id.part_id() != 0x7468
        || chip_id.mfg_id() != 0x74
        || chip_id.one() != 0x1
    {
        return Err(VscError::BadChipId(chip_id.into()));
    }

    Ok(())
}

#[export_name = "main"]
fn main() -> ! {
    ringbuf_entry!(Trace::Start(sys_get_timer().now));
    let spi = Spi::from(SPI.get_task_id()).device(VSC7448_SPI_DEVICE);
    let vsc7448 = Vsc7448Spi(spi);

    loop {
        match init(&vsc7448) {
            Ok(()) => {
                ringbuf_entry!(Trace::Initialized(sys_get_timer().now));
                break;
            }
            Err(e) => {
                ringbuf_entry!(Trace::FailedToInitialize(e));
                hl::sleep_for(200);
            }
        }
    }

    loop {
        hl::sleep_for(10);
    }
}
