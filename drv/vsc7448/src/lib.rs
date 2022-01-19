// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]

pub mod bsp;
pub mod spi;

mod dev;
mod port;
mod serdes10g;
mod serdes1g;
mod serdes6g;
mod spi_phy;

use userlib::hl::sleep_for;
use vsc7448_pac::Vsc7448;
pub use vsc_err::VscError;

/// Performs initial configuration (endianness, soft reset, read padding) of
/// the VSC7448, checks that its chip ID is correct, and brings core systems
/// out of reset.
pub fn init(v: &crate::spi::Vsc7448Spi) -> Result<(), VscError> {
    // Write the byte ordering / endianness configuration
    v.write(
        Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(),
        0x81818181.into(),
    )?;

    // Trigger a soft reset
    v.write(Vsc7448::DEVCPU_GCB().CHIP_REGS().SOFT_RST(), 1.into())?;

    // Re-write byte ordering / endianness
    v.write(
        Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CTRL(),
        0x81818181.into(),
    )?;
    // Configure reads to include 1 padding byte, since we're reading quickly
    v.write(Vsc7448::DEVCPU_ORG().DEVCPU_ORG().IF_CFGSTAT(), 1.into())?;

    let chip_id = v.read(Vsc7448::DEVCPU_GCB().CHIP_REGS().CHIP_ID())?;
    if chip_id.rev_id() != 0x3
        || chip_id.part_id() != 0x7468
        || chip_id.mfg_id() != 0x74
        || chip_id.one() != 0x1
    {
        return Err(VscError::BadChipId(chip_id.into()));
    }

    // Core chip bringup, bringing all of the main subsystems out of reset
    // (based on `jr2_init_conf_set` in the SDK)
    v.modify(Vsc7448::ANA_AC().STAT_GLOBAL_CFG_PORT().STAT_RESET(), |r| {
        r.set_reset(1)
    })?;
    v.modify(Vsc7448::ASM().CFG().STAT_CFG(), |r| {
        r.set_stat_cnt_clr_shot(1)
    })?;
    v.modify(Vsc7448::QSYS().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    v.modify(Vsc7448::REW().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    // The VOP isn't in the datasheet, but it's in the SDK
    v.modify(Vsc7448::VOP().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    v.modify(Vsc7448::ANA_AC().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    v.modify(Vsc7448::ASM().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;
    v.modify(Vsc7448::DSM().RAM_CTRL().RAM_INIT(), |r| {
        r.set_ram_init(1);
        r.set_ram_ena(1);
    })?;

    sleep_for(1);
    // TODO: read back all of those autoclear bits and make sure they cleared

    // Enable the queue system
    v.write_with(Vsc7448::QSYS().SYSTEM().RESET_CFG(), |r| r.set_core_ena(1))?;

    sleep_for(105); // Minimum time between reset and SMI access

    Ok(())
}
