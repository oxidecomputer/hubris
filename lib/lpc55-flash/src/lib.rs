// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
use lpc55_romapi::*;

pub use drv_update_api::UpdateTarget;
pub use lpc55_romapi::FLASH_PAGE_SIZE;

#[repr(u32)]
#[derive(Eq, PartialEq, Clone, Copy)]
pub enum HypoStatus {
    Success,
    RunningImage,
    OutOfBounds,
    FlashError(FlashStatus),
}

// All these symbols are defined with no space allocated. This is best
// represented as a zero sized type and accessed via the helpful macros
// below. () is not a valid C type but we don't actually care about about
// compatibility with C here.
#[allow(improper_ctypes)]
extern "C" {
    static __IMAGE_A_BASE: ();
    static __IMAGE_A_END: ();

    static __IMAGE_B_BASE: ();
    static __IMAGE_B_END: ();

    static __IMAGE_STAGE0_BASE: ();
    static __IMAGE_STAGE0_END: ();

    // This references the base of the currently running image
    static __this_image: ();
}

macro_rules! this_image {
    () => {
        core::ptr::addr_of!(__this_image) as u32
    };
}

macro_rules! image_a_base {
    () => {
        core::ptr::addr_of!(__IMAGE_A_BASE) as u32
    };
}

macro_rules! image_a_end {
    () => {
        core::ptr::addr_of!(__IMAGE_A_END) as u32
    };
}

macro_rules! image_b_base {
    () => {
        core::ptr::addr_of!(__IMAGE_B_BASE) as u32
    };
}

macro_rules! image_b_end {
    () => {
        core::ptr::addr_of!(__IMAGE_B_END) as u32
    };
}

macro_rules! image_stage0_base {
    () => {
        core::ptr::addr_of!(__IMAGE_STAGE0_BASE) as u32
    };
}

macro_rules! image_stage0_end {
    () => {
        core::ptr::addr_of!(__IMAGE_STAGE0_END) as u32
    };
}

fn get_base(which: UpdateTarget) -> u32 {
    match which {
        UpdateTarget::ImageA => unsafe { image_a_base!() },
        UpdateTarget::ImageB => unsafe { image_b_base!() },
        UpdateTarget::Bootloader => unsafe { image_stage0_base!() },
        _ => unreachable!(),
    }
}

fn get_end(which: UpdateTarget) -> u32 {
    match which {
        UpdateTarget::ImageA => unsafe { image_a_end!() },
        UpdateTarget::ImageB => unsafe { image_b_end!() },
        UpdateTarget::Bootloader => unsafe { image_stage0_end!() },
        _ => unreachable!(),
    }
}

fn same_image(which: UpdateTarget) -> bool {
    get_base(which) == unsafe { this_image!() }
}

fn target_addr(
    image_target: UpdateTarget,
    page_num: u32,
) -> Result<u32, HypoStatus> {
    let base = get_base(image_target);

    // This is safely calculating addr = base + page_num * PAGE_SIZE
    let addr = page_num
        .checked_mul(lpc55_romapi::FLASH_PAGE_SIZE as u32)
        .and_then(|product| product.checked_add(base))
        .ok_or(HypoStatus::OutOfBounds)?;

    // check addr + PAGE_SIZE < end
    if addr
        .checked_add(lpc55_romapi::FLASH_PAGE_SIZE as u32)
        .ok_or(HypoStatus::OutOfBounds)?
        > get_end(image_target)
    {
        return Err(HypoStatus::OutOfBounds);
    }

    Ok(addr)
}

#[no_mangle]
pub unsafe extern "C" fn __write_block(
    image_num: UpdateTarget,
    page_num: u32,
    buffer: *mut u8,
) -> HypoStatus {
    // Can only update opposite image
    if same_image(image_num) {
        return HypoStatus::RunningImage;
    }

    let write_addr = match target_addr(image_num, page_num) {
        Ok(addr) => addr,
        Err(e) => return e,
    };

    // We expect this to be called from non-secure (running on 28) and
    // non-privileged mode (called from hubris task). The tt instructions
    // are mostly useless for doing any kind of checking on the buffer
    // address passed in. The failure mode is going to be a fault.

    // TODO: Is there a cost (flash wear) to erasing an already erased
    // block or is the cost only incurred on the subsequent write?
    if let Err(result) = flash_erase(write_addr, FLASH_PAGE_SIZE as u32) {
        return HypoStatus::FlashError(result);
    }

    if let Err(result) = flash_write(write_addr, buffer, FLASH_PAGE_SIZE as u32)
    {
        return HypoStatus::FlashError(result);
    }

    HypoStatus::Success
}

#[no_mangle]
pub unsafe extern "C" fn __erase_block(
    image_num: UpdateTarget,
    page_num: u32,
) -> HypoStatus {
    // Can only update opposite image
    if same_image(image_num) {
        return HypoStatus::RunningImage;
    }

    // TODO: Is there a cost (flash wear) to erasing an already erased
    // block or is the cost only incurred on the subsequent write?
    let erase_addr = match target_addr(image_num, page_num) {
        Ok(addr) => addr,
        Err(e) => return e,
    };

    // We expect this to be called from non-secure (running on 28) and
    // non-privileged mode (called from hubris task). The tt instructions
    // are mostly useless for doing any kind of checking on the buffer
    // address passed in. The failure mode is going to be a fault.

    if let Err(result) = flash_erase(erase_addr, FLASH_PAGE_SIZE as u32) {
        return HypoStatus::FlashError(result);
    }

    HypoStatus::Success
}
