#![no_std]
#![no_main]

mod bsp;
mod serdes10g;
mod vsc7448_spi;

use drv_spi_api::{Spi, SpiError};
use userlib::*;
use vsc7448_pac::Vsc7448;
use vsc7448_spi::Vsc7448Spi;

cfg_if::cfg_if! {
    if #[cfg(target_board = "gemini-bu-1")] {
        use bsp::gemini_bu::Bsp;
    } else {
        use bsp::empty::Bsp;
    }
}

task_slot!(SPI, spi_driver);
const VSC7448_SPI_DEVICE: u8 = 0;

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq)]
pub enum VscError {
    SpiError(SpiError),
    BadChipId(u32),
    MiimReadErr {
        miim: u8,
        phy: u8,
        page: u16,
        addr: u8,
    },
    BadPhyId1(u16),
    BadPhyId2(u16),
    MiimIdleTimeout,
    MiimReadTimeout,
    Serdes6gReadTimeout {
        instance: u16,
    },
    Serdes6gWriteTimeout {
        instance: u16,
    },
    PortFlushTimeout {
        port: u8,
    },
    AnaCfgTimeout,
    SerdesFrequencyTooLow(u64),
    SerdesFrequencyTooHigh(u64),
    TriDecFailed(u16),
    BiDecFailed(u16),
    LtDecFailed(u16),
    LsDecFailed(u16),
    TxPllLockFailed,
    TxPllFsmFailed,
    RxPllLockFailed,
    RxPllFsmFailed,
    OffsetCalFailed,
}

impl From<SpiError> for VscError {
    fn from(s: SpiError) -> Self {
        Self::SpiError(s)
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Performs initial configuration (endianness, soft reset, read padding) of
/// the VSC7448, then checks that its chip ID is correct.
fn init(vsc7448: &Vsc7448Spi) -> Result<Bsp, VscError> {
    // Write the byte ordering / endianness configuration
    vsc7448.write(
        Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(),
        0x81818181.into(),
    )?;

    // Trigger a soft reset
    vsc7448.write(Vsc7448::DEVCPU_GCB().CHIP_REGS().SOFT_RST(), 1.into())?;

    // Re-write byte ordering / endianness
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

    Bsp::new(vsc7448)
}

#[export_name = "main"]
fn main() -> ! {
    let spi = Spi::from(SPI.get_task_id()).device(VSC7448_SPI_DEVICE);
    let vsc7448 = Vsc7448Spi(spi);

    loop {
        match init(&vsc7448) {
            Ok(bsp) => bsp.run(), // Does not terminate
            Err(_e) => hl::sleep_for(200),
        }
    }
}
