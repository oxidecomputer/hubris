#![no_std]
#![no_main]

mod bsp;

use drv_spi_api::Spi;
use userlib::*;
use vsc7448::{spi::Vsc7448Spi, VscError};
use vsc7448_pac::Vsc7448;

cfg_if::cfg_if! {
    if #[cfg(target_board = "gemini-bu-1")] {
        use bsp::gemini_bu::Bsp;
    } else if #[cfg(target_board = "sidecar-1")] {
        use bsp::sidecar::Bsp;
    } else {
        compile_error!("No BSP available for this board");
    }
}

task_slot!(SPI, spi_driver);
const VSC7448_SPI_DEVICE: u8 = 0;

////////////////////////////////////////////////////////////////////////////////

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

    // Core chip bringup, bringing all of the main subsystems out of reset
    // (based on `jr2_init_conf_set` in the SDK)
    vsc7448
        .modify(Vsc7448::ANA_AC().STAT_GLOBAL_CFG_PORT().STAT_RESET(), |r| {
            r.set_reset(1)
        })?;
    vsc7448.modify(Vsc7448::ASM().CFG().STAT_CFG(), |r| {
        r.set_stat_cnt_clr_shot(1)
    })?;
    vsc7448.modify(Vsc7448::QSYS().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    vsc7448.modify(Vsc7448::REW().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    // The VOP isn't in the datasheet, but it's in the SDK
    vsc7448.modify(Vsc7448::VOP().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    vsc7448.modify(Vsc7448::ANA_AC().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    vsc7448.modify(Vsc7448::ASM().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    vsc7448.modify(Vsc7448::DSM().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;

    hl::sleep_for(1);
    // TODO: read back all of those autoclear bits and make sure they cleared

    // Enable the queue system
    vsc7448.write_with(Vsc7448::QSYS().SYSTEM().RESET_CFG(), |r| {
        r.set_core_ena(1)
    })?;

    hl::sleep_for(105); // Minimum time between reset and SMI access

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
