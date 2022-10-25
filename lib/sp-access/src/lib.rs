// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
#![no_std]

use abi::ImageHeader;
use drv_sp_ctrl_api::*;
use zerocopy::FromBytes;

// Keys constants are defined in RM0433 Rev 7
// Section 4.9.2
const FLASH_KEY1: u32 = 0x4567_0123;
const FLASH_KEY2: u32 = 0xCDEF_89AB;

// Keys constants are defined in RM0433 Rev 7
// Section 4.9.3
const FLASH_OPT_KEY1: u32 = 0x0819_2A3B;
const FLASH_OPT_KEY2: u32 = 0x4C5D_6E7F;

const FLASH_OPT_KEYR: u32 = 0x5200_2008;
const FLASH_OPT_CR: u32 = 0x5200_2018;
const FLASH_OPTSR_CUR: u32 = 0x5200_201C;
const FLASH_OPTSR_PRG: u32 = 0x5200_2020;

const FLASH_KEYR1: u32 = 0x5200_2004;
const FLASH_CR1: u32 = 0x5200_200C;
const FLASH_SR1: u32 = 0x5200_2010;

const FLASH_KEYR2: u32 = 0x5200_2104;
const FLASH_CR2: u32 = 0x5200_210C;
const FLASH_SR2: u32 = 0x5200_2110;

const ACTIVE_BANK_ADDR: u32 = 0x08000000;
const PENDING_BANK_ADDR: u32 = 0x08100000;
// Hmmmm, would be nice to autogenerate this somehow
const VECTOR_SIZE: u32 = 0x298;

//const HEADER_ADDR: u32 = BANK_ADDR + VECTOR_SIZE;

const DHCSR: u32 = 0xE000EDF0;
// [citation needed] set the magic value, enter debug mode and halt the
// processor
const DHCSR_HALT_MAGIC: u32 = 0xa05f_0003;

fn halt(sp_ctrl: &SpCtrl) -> Result<(), SpCtrlError> {
    sp_ctrl.write_word_32(DHCSR, DHCSR_HALT_MAGIC)
}

fn unlock_option(sp_ctrl: &SpCtrl) -> Result<(), SpCtrlError> {
    let v = sp_ctrl.read_word_32(FLASH_CR1)?;
    if (v & 1) == 1 {
        sp_ctrl.write_word_32(FLASH_KEYR1, FLASH_KEY1)?;
        sp_ctrl.write_word_32(FLASH_KEYR1, FLASH_KEY2)?;
    }

    let v = sp_ctrl.read_word_32(FLASH_CR2)?;
    if (v & 1) == 1 {
        sp_ctrl.write_word_32(FLASH_KEYR2, FLASH_KEY1)?;
        sp_ctrl.write_word_32(FLASH_KEYR2, FLASH_KEY2)?;
    }

    let v = sp_ctrl.read_word_32(FLASH_OPT_CR)?;
    if (v & 1) == 1 {
        sp_ctrl.write_word_32(FLASH_OPT_KEYR, FLASH_OPT_KEY1)?;
        sp_ctrl.write_word_32(FLASH_OPT_KEYR, FLASH_OPT_KEY2)?;
    }
    Ok(())
}

fn commit_option(sp_ctrl: &SpCtrl) -> Result<(), SpCtrlError> {
    // set start bit
    sp_ctrl.write_word_32(FLASH_OPT_CR, 0x2)?;

    loop {
        let stat = sp_ctrl.read_word_32(FLASH_OPTSR_CUR)?;
        if (stat & 0x1) == 0 {
            break;
        }
    }
    Ok(())
}

fn image_version(
    sp_ctrl: &SpCtrl,
    base: u32,
) -> Result<Option<(u32, u32)>, SpCtrlError> {
    const HEADER_SIZE: usize = core::mem::size_of::<ImageHeader>();

    let mut header_bytes: [u8; HEADER_SIZE] = [0; HEADER_SIZE];

    sp_ctrl.read_transaction_start(base, base + HEADER_SIZE as u32)?;

    sp_ctrl.read_transaction(&mut header_bytes)?;

    let header: ImageHeader =
        ImageHeader::read_from(&header_bytes[..]).unwrap();

    if header.magic != abi::HEADER_MAGIC {
        return Ok(None);
    }

    Ok(Some((header.epoch, header.version)))
}

pub fn active_image_version(
    sp_ctrl: &SpCtrl,
) -> Result<Option<(u32, u32)>, SpCtrlError> {
    image_version(sp_ctrl, ACTIVE_BANK_ADDR + VECTOR_SIZE)
}

pub fn pending_image_version(
    sp_ctrl: &SpCtrl,
) -> Result<Option<(u32, u32)>, SpCtrlError> {
    image_version(sp_ctrl, PENDING_BANK_ADDR + VECTOR_SIZE)
}

pub fn swap_bank(sp_ctrl: &SpCtrl) -> Result<(), SpCtrlError> {
    halt(sp_ctrl)?;

    unlock_option(sp_ctrl)?;

    const SWAP_BANK_BIT: u32 = 0x8000_0000;

    let optsr = sp_ctrl.read_word_32(FLASH_OPTSR_CUR)?;

    if (optsr & SWAP_BANK_BIT) == SWAP_BANK_BIT {
        sp_ctrl.write_word_32(FLASH_OPTSR_PRG, optsr & !SWAP_BANK_BIT)?;
    } else {
        sp_ctrl.write_word_32(FLASH_OPTSR_PRG, optsr | SWAP_BANK_BIT)?;
    }

    commit_option(sp_ctrl)?;
    Ok(())
}

fn bank_erase(sp_ctrl: &SpCtrl, cr: u32, sr: u32) -> Result<(), SpCtrlError> {
    halt(sp_ctrl)?;

    unlock_option(sp_ctrl)?;

    const ERASE_BITS: u32 = 0b1000_1000;

    sp_ctrl.write_word_32(cr, ERASE_BITS)?;

    loop {
        let v = sp_ctrl.read_word_32(sr)?;

        const QW: u32 = 0b100;

        if (v & QW) == QW {
            break;
        }
    }

    // TODO check some error bits
    Ok(())
}

pub fn active_bank_erase(sp_ctrl: &SpCtrl) -> Result<(), SpCtrlError> {
    bank_erase(sp_ctrl, FLASH_CR1, FLASH_SR1)
}

pub fn pending_bank_erase(sp_ctrl: &SpCtrl) -> Result<(), SpCtrlError> {
    bank_erase(sp_ctrl, FLASH_CR2, FLASH_SR2)
}

// Swap from the active image back to the pending image (after checking versions)
// Steps
// - Make sure processor is halted
// - Erase active bank
// - Bank swap
//
// The next time the SP is reset the pending image is now active and the pending
// area is blank.
//
// XXX What does error conditions look like?

enum RollbackError {
    NoImage,
    EpochFail,
}

pub fn rollback(sp_ctrl: &SpCtrl) -> Result<(), SpCtrlError> {
    let (active_epoch, _active_version) = match active_image_version(sp_ctrl) {
        Ok(f) => match f {
            Some((e, v)) => (e, v),
            None => panic!(),
        },
        _ => panic!(),
    };

    let (pending_epoch, _pending_version) =
        match pending_image_version(&sp_ctrl) {
            Ok(f) => match f {
                Some((e, v)) => (e, v),
                None => panic!(),
            },
            _ => panic!(),
        };

    if pending_epoch < active_epoch {
        panic!();
    }

    active_bank_erase(sp_ctrl)?;

    swap_bank(sp_ctrl)?;

    Ok(())
}
