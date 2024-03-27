// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::ops::Range;
use drv_lpc55_update_api::BLOCK_SIZE_BYTES;
use drv_lpc55_update_api::{RotComponent, SlotId};
use drv_update_api::UpdateError;
use userlib::UnwrapLite;

// We shouldn't actually dereference these. The types are not correct.
// They are just here to allow a mechanism for getting the addresses.
extern "C" {
    static __IMAGE_A_BASE: [u32; 0];
    static __IMAGE_B_BASE: [u32; 0];
    static __IMAGE_STAGE0_BASE: [u32; 0];
    static __IMAGE_STAGE0NEXT_BASE: [u32; 0];
    static __IMAGE_A_END: [u32; 0];
    static __IMAGE_B_END: [u32; 0];
    static __IMAGE_STAGE0_END: [u32; 0];
    static __IMAGE_STAGE0NEXT_END: [u32; 0];

    static __this_image: [u32; 0];
}

// Location of the NXP header
pub const HEADER_BLOCK: usize = 0;

// NXP LPC55's mixed header/vector table offsets
const RESET_VECTOR_OFFSET: usize = 0x04;
pub const LENGTH_OFFSET: usize = 0x20;
pub const HEADER_OFFSET: u32 = 0x130;
const MAGIC_OFFSET: usize = HEADER_OFFSET as usize;

// Perform some sanity checking on the header block.
pub fn validate_header_block(
    component: RotComponent,
    slot: SlotId,
    block: &[u8; BLOCK_SIZE_BYTES],
) -> Result<(), UpdateError> {
    let exec = image_range(component, slot).1;

    // This part aliases flash in two positions that differ in bit 28. To allow
    // for either position to be used in new images, we clear bit 28 in all of
    // the numbers used for comparison below, by ANDing them with this mask:
    const ADDRMASK: u32 = !(1 << 28);

    let reset_vector = u32::from_le_bytes(
        block[RESET_VECTOR_OFFSET..][..4].try_into().unwrap_lite(),
    ) & ADDRMASK;

    // Ensure the image is destined for the right target
    if !exec.contains(&reset_vector) {
        return Err(UpdateError::InvalidHeaderBlock);
    }

    // Ensure the MAGIC is correct.
    // Bootloaders have been released without an ImageHeader. Allow those.
    let magic =
        u32::from_le_bytes(block[MAGIC_OFFSET..][..4].try_into().unwrap_lite());
    if component == RotComponent::Hubris && magic != abi::HEADER_MAGIC {
        return Err(UpdateError::InvalidHeaderBlock);
    }

    Ok(())
}

pub fn same_image(component: RotComponent, slot: SlotId) -> bool {
    // Safety: We are trusting the linker.
    image_range(component, slot).0.start
        == unsafe { &__this_image } as *const _ as u32
}

/// Return the flash storage address range and flash execution address range.
/// These are only different for the staged stage0 image.
pub fn image_range(
    component: RotComponent,
    slot: SlotId,
) -> (Range<u32>, Range<u32>) {
    unsafe {
        match (component, slot) {
            (RotComponent::Hubris, SlotId::A) => (
                __IMAGE_A_BASE.as_ptr() as u32..__IMAGE_A_END.as_ptr() as u32,
                __IMAGE_A_BASE.as_ptr() as u32..__IMAGE_A_END.as_ptr() as u32,
            ),
            (RotComponent::Hubris, SlotId::B) => (
                __IMAGE_B_BASE.as_ptr() as u32..__IMAGE_B_END.as_ptr() as u32,
                __IMAGE_B_BASE.as_ptr() as u32..__IMAGE_B_END.as_ptr() as u32,
            ),
            (RotComponent::Stage0, SlotId::A) => (
                __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
                __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
            ),
            (RotComponent::Stage0, SlotId::B) => (
                __IMAGE_STAGE0NEXT_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0NEXT_END.as_ptr() as u32,
                __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
            ),
        }
    }
}
