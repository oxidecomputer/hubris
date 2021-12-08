#![no_std]
#![no_main]

// NOTE: you will probably want to remove this when you write your actual code;
// we need to import userlib to get this to compile, but it throws a warning
// because we're not actually using it yet!
use ringbuf::*;
#[allow(unused_imports)]
use userlib::*;

use drv_spi_api::{Spi, SpiDevice, SpiError};
use vsc7448_pac::{types::RegisterAddress, Vsc7448};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Read(u32, u32),
    Write(u32, u32),
    Initialized,
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
}

impl From<SpiError> for VscError {
    fn from(s: SpiError) -> Self {
        Self::SpiError(s)
    }
}

/// Helper struct to read and write from the VSC7448 over SPI
struct Vsc7448Spi(SpiDevice);
impl Vsc7448Spi {
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

        ringbuf_entry!(Trace::Read(reg.addr, value));
        Ok(value.into())
    }
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

        ringbuf_entry!(Trace::Write(reg.addr, value.into()));
        self.0.write(&data[..])?;
        Ok(())
    }
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
}

////////////////////////////////////////////////////////////////////////////////

fn init(vsc7448: &Vsc7448Spi) -> Result<(), VscError> {
    // Write the byte ordering / endianness configuration
    vsc7448.write(
        Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(),
        0x81818181.into(),
    )?;
    // Configure reads to include 1 padding byte, since we're reading quickly
    vsc7448.write(Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CFGSTAT(), 1.into())?;

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
    let spi = Spi::from(SPI.get_task_id()).device(VSC7448_SPI_DEVICE);
    let vsc7448 = Vsc7448Spi(spi);

    loop {
        match init(&vsc7448) {
            Ok(()) => {
                ringbuf_entry!(Trace::Initialized);
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
