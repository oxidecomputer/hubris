// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// Implement LPC55_`update_server`'s knowledge about bits inside an image.
///
/// The update server needs to work with partial or otherwise invalid images.
/// Signature checks are only performed at boot time. The update server
/// does match FWIDs against the boot-time-info in some cases. But, because
/// flash areas are mutated during update_server operations and stuff can
/// happen, any data in the non-active Hubris image needs to be treated as
/// untrusted (update_server does not alter its own image).
/// Data structures, pointers, and offsets within an image are tested to
/// ensure no mischief during `update_server` operations. The remainder
/// of reliability and security concerns rely on the boot-time policies
/// of the LPC55 ROM and the stage0 bootloader.
use crate::{
    indirect_flash_read, round_up_to_flash_page, SIZEOF_U32, U32_SIZE,
};
use abi::{ImageHeader, CABOOSE_MAGIC, HEADER_MAGIC};
use core::ops::Range;
use core::ptr::addr_of;
use drv_lpc55_update_api::{RawCabooseError, RotComponent, SlotId};
use drv_update_api::UpdateError;
use zerocopy::{AsBytes, FromBytes};

// Our layout of flash banks on the LPC55.
// Addresses are from the linker.
// The bootloader (`bootleby`) resides at __STAGE0_BASE and
// only references IMAGE_A and IMAGE_B.
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

// An image may have an ImageHeader located after the
// LPC55's mixed header/vector table.
pub const IMAGE_HEADER_OFFSET: u32 = 0x130;

/// Address ranges that may contain an image during storage and active use.
/// `stored` and `at_runtime` ranges are the same except for `stage0next`.
// TODO: Make these RangeInclusive in case we ever need to model
// some slot at the end of the  address space.
pub struct FlashRange {
    pub stored: Range<u32>,
    pub at_runtime: Range<u32>,
}

/// Get the flash storage address range and flash execution address range.
pub fn flash_range(component: RotComponent, slot: SlotId) -> FlashRange {
    // Safety: this block requires unsafe code to generate references to the
    // extern "C" statics. Because we're only getting their addresses (all
    // operations below are just as_ptr), we can't really trigger any UB here.
    // The addresses themselves are assumed to be valid because they're
    // produced by the linker, which we implicitly trust.
    unsafe {
        match (component, slot) {
            (RotComponent::Hubris, SlotId::A) => FlashRange {
                stored: __IMAGE_A_BASE.as_ptr() as u32
                    ..__IMAGE_A_END.as_ptr() as u32,
                at_runtime: __IMAGE_A_BASE.as_ptr() as u32
                    ..__IMAGE_A_END.as_ptr() as u32,
            },
            (RotComponent::Hubris, SlotId::B) => FlashRange {
                stored: __IMAGE_B_BASE.as_ptr() as u32
                    ..__IMAGE_B_END.as_ptr() as u32,
                at_runtime: __IMAGE_B_BASE.as_ptr() as u32
                    ..__IMAGE_B_END.as_ptr() as u32,
            },
            (RotComponent::Stage0, SlotId::A) => FlashRange {
                stored: __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
                at_runtime: __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
            },
            (RotComponent::Stage0, SlotId::B) => FlashRange {
                stored: __IMAGE_STAGE0NEXT_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0NEXT_END.as_ptr() as u32,
                at_runtime: __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
            },
        }
    }
}

/// Does (component, slot) refer to the currently running Hubris image?
pub fn is_current_hubris_image(component: RotComponent, slot: SlotId) -> bool {
    // Safety: extern statics aren't controlled by Rust so poking them can
    // cause UB; in this case, it's zero length and we are only taking its
    // numerical address, so we're not at risk.
    flash_range(component, slot).stored.start == addr_of!(__this_image) as u32
}

// LPC55 defined image content

/// Image header for the LPC55S6x device as documented in NXP UM11126
#[repr(C)]
#[derive(Default, AsBytes, FromBytes)]
pub struct ImageVectorsLpc55 {
    initial_sp: u32,                    // 0x00
    initial_pc: u32,                    // 0x04
    _vector_table_0: [u32; 6],          // 0x08, 0c, 10, 14, 18, 1c
    nxp_image_length: u32,              // 0x20
    nxp_image_type: u32,                // 0x24
    nxp_offset_to_specific_header: u32, // 0x28
    _vector_table_1: [u32; 2],          // 0x2c, 0x30
    nxp_image_executation_address: u32, // 0x32
                                        // Additional trailing vectors are not
                                        // interesting here.
                                        // _vector_table_2[u32; 2]
}

impl ImageVectorsLpc55 {
    const IMAGE_TYPE_PLAIN_SIGNED_XIP_IMAGE: u32 = 4;

    pub fn is_image_type_signed_xip(&self) -> bool {
        self.nxp_image_type == Self::IMAGE_TYPE_PLAIN_SIGNED_XIP_IMAGE
    }

    // This part aliases flash in two positions that differ in bit 28. To allow
    // for either position to be used in new images, we clear bit 28 in all of
    // the numbers used for comparison below, by ANDing them with this mask:
    pub fn normalized_initial_pc(&self) -> u32 {
        const ADDRMASK: u32 = !(1 << 28);
        self.initial_pc & ADDRMASK
    }

    // Length of image from offset zero to end of the signature block (without padding)
    // a.k.a. ImageVectorsLpc55.nxp_image_length
    pub fn image_length(&self) -> Option<u32> {
        if self.is_image_type_signed_xip() {
            Some(self.nxp_image_length)
        } else {
            None
        }
    }

    /// Image length padded to nearest page size.
    pub fn padded_image_len(&self) -> Option<u32> {
        round_up_to_flash_page(self.image_length()?)
    }

    /// Determine the bounds of an image assuming the given flash bank
    /// addresses.
    pub fn padded_image_range(
        &self,
        at_runtime: &Range<u32>,
    ) -> Option<Range<u32>> {
        let image_start = at_runtime.start;
        let image_end =
            at_runtime.start.checked_add(self.padded_image_len()?)?;
        Some(image_start..image_end)
    }
}

impl TryFrom<&[u8]> for ImageVectorsLpc55 {
    type Error = ();

    fn try_from(buffer: &[u8]) -> Result<Self, Self::Error> {
        match ImageVectorsLpc55::read_from_prefix(buffer) {
            Some(vectors) => Ok(vectors),
            None => Err(()),
        }
    }
}

/// Sanity check the image header block.
/// Return the offset to the end of the executable code which is also
/// the end of optional caboose and the beginning of the signature block.
pub fn validate_header_block(
    header_access: &ImageAccess<'_>,
) -> Result<u32, UpdateError> {
    let mut vectors = ImageVectorsLpc55::new_zeroed();
    let mut header = ImageHeader::new_zeroed();

    // Read block 0 and the header contained within (if available).
    if header_access.read_bytes(0, vectors.as_bytes_mut()).is_err()
        || header_access
            .read_bytes(IMAGE_HEADER_OFFSET, header.as_bytes_mut())
            .is_err()
    {
        return Err(UpdateError::InvalidHeaderBlock);
    }

    // Check image type and presence of signature block.
    if !vectors.is_image_type_signed_xip()
        || vectors.nxp_offset_to_specific_header >= vectors.nxp_image_length
    {
        // Not a signed XIP image or no signature block.
        // If we figure out a reasonable minimum size for the signature block
        // we should test for that.
        return Err(UpdateError::InvalidHeaderBlock);
    }

    // We don't rely on the ImageHeader, but if it is there, it needs to be valid.
    // Note that `ImageHeader.epoch` is used by rollback protection for early
    // rejection of invalid images.
    // TODO: Improve estimate of where the first executable instruction can be.
    let code_offset = if header.magic == HEADER_MAGIC {
        if header.total_image_len != vectors.nxp_offset_to_specific_header {
            // ImageHeader disagrees with LPC55 vectors.
            return Err(UpdateError::InvalidHeaderBlock);
        }
        // Adding constants should be resolved at compile time: no call to panic.
        IMAGE_HEADER_OFFSET + (core::mem::size_of::<ImageHeader>() as u32)
    } else {
        IMAGE_HEADER_OFFSET
    };

    if vectors.nxp_image_length as usize > header_access.at_runtime().len() {
        // Image extends outside of flash bank.
        return Err(UpdateError::InvalidHeaderBlock);
    }

    // Check that the initial PC is pointing to a reasonable location.
    // We only have information from image block zero, so this is just
    // a basic sanity check.
    // A check at signing time can be more exact, but this helps reject
    // ridiculous images before any flash is erased.
    let caboose_end = header_access
        .at_runtime()
        .start
        .checked_add(vectors.nxp_offset_to_specific_header)
        .ok_or(UpdateError::InvalidHeaderBlock)?;
    let text_start = header_access
        .at_runtime()
        .start
        .checked_add(code_offset)
        .ok_or(UpdateError::InvalidHeaderBlock)?;
    if !(text_start..caboose_end).contains(&vectors.normalized_initial_pc()) {
        return Err(UpdateError::InvalidHeaderBlock);
    }

    Ok(vectors.nxp_offset_to_specific_header)
}

/// Get the range of the caboose contained within an image if it exists.
///
/// This implementation has similar logic to the one in `stm32h7-update-server`,
/// but uses ImageAccess for images that, during various operations,
/// may be in RAM, Flash, or split between both.
pub fn caboose_slice(
    image: &ImageAccess<'_>,
) -> Result<Range<u32>, RawCabooseError> {
    // The ImageHeader is optional since the offset to the start of
    // the signature block (end of image) is also found in an LPC55
    // Type 4 (Signed XIP) image.
    //
    // In this context, NoImageHeader actually means that the image
    // is not well formed.
    let image_end_offset = validate_header_block(image)
        .map_err(|_| RawCabooseError::NoImageHeader)?;

    // By construction, the last word of the caboose is its size as a `u32`
    let caboose_size_offset = image_end_offset
        .checked_sub(U32_SIZE)
        .ok_or(RawCabooseError::MissingCaboose)?;
    let caboose_size = image
        .read_word(caboose_size_offset)
        .map_err(|_| RawCabooseError::ReadFailed)?;

    // Security considerations:
    // A maliciously constructed image could be staged in flash
    // with an apparently large caboose that would allow some access
    // within its own flash slot. Presumably, we would never sign
    // such an image so the bootloader would never execute it.
    // However, reading out that image's caboose would be allowed.
    // The range and size checks on caboose access are meant to keep
    // accesses within the Hubris image which is constrained to its
    // flash slot.
    // There is no sensitive information to be found there.
    let caboose_magic_offset = image_end_offset
        .checked_sub(caboose_size)
        .ok_or(RawCabooseError::MissingCaboose)?;
    if ((caboose_magic_offset % U32_SIZE) != 0)
        || !(IMAGE_HEADER_OFFSET..caboose_size_offset)
            .contains(&caboose_magic_offset)
    {
        return Err(RawCabooseError::MissingCaboose);
    }

    let caboose_magic = image
        .read_word(caboose_magic_offset)
        .map_err(|_| RawCabooseError::MissingCaboose)?;

    if caboose_magic == CABOOSE_MAGIC {
        let caboose_start = caboose_magic_offset
            .checked_add(U32_SIZE)
            .ok_or(RawCabooseError::MissingCaboose)?;
        Ok(caboose_start..caboose_size_offset)
    } else {
        Err(RawCabooseError::MissingCaboose)
    }
}

/// Accessor keeps the implementation details of ImageAccess private
enum Accessor<'a> {
    // Flash driver, flash device range
    Flash {
        flash: &'a drv_lpc55_flash::Flash<'a>,
        span: FlashRange,
    },
    Ram {
        buffer: &'a [u8],
        span: FlashRange,
    },
    // Hybrid is used for later implementation of rollback protection.
    // The buffer is used in place of the beginning of the flash range.
    _Hybrid {
        buffer: &'a [u8],
        flash: &'a drv_lpc55_flash::Flash<'a>,
        span: FlashRange,
    },
}

impl Accessor<'_> {
    fn at_runtime(&self) -> &Range<u32> {
        match self {
            Accessor::Flash { span, .. }
            | Accessor::Ram { span, .. }
            | Accessor::_Hybrid { span, .. } => &span.at_runtime,
        }
    }
}

/// In addition to images that are located in their respective
/// flash slots, the `update_server` needs to read data from
/// complete and partial images in RAM or split between RAM
/// and flash.
/// The specific cases are when the
///   - image is entirely in flash.
///   - header block is in RAM with the remainder unavailable.
///   - header block is in RAM with the remainder in flash.
///   - entire image is in RAM (in the case of a cached Stage0 image).
///
/// Calls to methods use offsets into the image which is helpful
/// when dealing with the offsets and sizes found in image headers
/// and the caboose.
pub struct ImageAccess<'a> {
    accessor: Accessor<'a>,
}

impl ImageAccess<'_> {
    pub fn new_flash<'a>(
        flash: &'a drv_lpc55_flash::Flash<'a>,
        component: RotComponent,
        slot: SlotId,
    ) -> ImageAccess<'a> {
        let span = flash_range(component, slot);
        ImageAccess {
            accessor: Accessor::Flash { flash, span },
        }
    }

    pub fn new_ram(
        buffer: &[u8],
        component: RotComponent,
        slot: SlotId,
    ) -> ImageAccess<'_> {
        let span = flash_range(component, slot);
        ImageAccess {
            accessor: Accessor::Ram { buffer, span },
        }
    }

    pub fn _new_hybrid<'a>(
        flash: &'a drv_lpc55_flash::Flash<'a>,
        buffer: &'a [u8],
        component: RotComponent,
        slot: SlotId,
    ) -> ImageAccess<'a> {
        let span = flash_range(component, slot);
        ImageAccess {
            accessor: Accessor::_Hybrid {
                flash,
                buffer,
                span,
            },
        }
    }

    fn at_runtime(&self) -> &Range<u32> {
        self.accessor.at_runtime()
    }

    /// True if the u32 at offset is contained within the slot.
    pub fn is_addressable(&self, offset: u32) -> bool {
        let len = self.at_runtime().len() as u32;
        if let Some(end) = offset.checked_add(U32_SIZE) {
            end <= len
        } else {
            false
        }
    }

    /// Fetch a u32 from an image.
    pub fn read_word(&self, offset: u32) -> Result<u32, UpdateError> {
        if !self.is_addressable(offset) {
            return Err(UpdateError::OutOfBounds);
        }
        match &self.accessor {
            Accessor::Flash { flash, span } => {
                let addr = span
                    .stored
                    .start
                    .checked_add(offset)
                    .ok_or(UpdateError::OutOfBounds)?;
                let mut word = 0u32;
                indirect_flash_read(flash, addr, word.as_bytes_mut())?;
                Ok(word)
            }
            Accessor::Ram { buffer, .. } => {
                let word_end = (offset as usize)
                    .checked_add(SIZEOF_U32)
                    .ok_or(UpdateError::OutOfBounds)?;
                Ok(buffer
                    .get(offset as usize..word_end)
                    .and_then(u32::read_from)
                    .ok_or(UpdateError::OutOfBounds)?)
            }
            Accessor::_Hybrid {
                buffer,
                flash,
                span,
            } => {
                if (offset as usize) < buffer.len() {
                    // Word is in the RAM portion
                    let word_end = (offset as usize)
                        .checked_add(SIZEOF_U32)
                        .ok_or(UpdateError::OutOfBounds)?;
                    Ok(buffer
                        .get(offset as usize..word_end)
                        .and_then(u32::read_from)
                        .ok_or(UpdateError::OutOfBounds)?)
                } else {
                    let addr = span
                        .stored
                        .start
                        .checked_add(offset)
                        .ok_or(UpdateError::OutOfBounds)?;
                    let mut word = 0u32;
                    indirect_flash_read(flash, addr, word.as_bytes_mut())?;
                    Ok(word)
                }
            }
        }
    }

    pub fn read_bytes(
        &self,
        offset: u32,
        buffer: &mut [u8],
    ) -> Result<(), UpdateError> {
        let len = buffer.len() as u32;
        match &self.accessor {
            Accessor::Flash { flash, span } => {
                let start = span
                    .stored
                    .start
                    .checked_add(offset)
                    .ok_or(UpdateError::OutOfBounds)?;
                let end =
                    start.checked_add(len).ok_or(UpdateError::OutOfBounds)?;
                if span.stored.contains(&start)
                    && (span.stored.start..=span.stored.end).contains(&end)
                {
                    Ok(indirect_flash_read(flash, start, buffer)?)
                } else {
                    Err(UpdateError::OutOfBounds)
                }
            }
            Accessor::Ram { buffer: src, .. } => {
                let end =
                    offset.checked_add(len).ok_or(UpdateError::OutOfBounds)?;
                if let Some(data) = src.get((offset as usize)..(end as usize)) {
                    buffer.copy_from_slice(data);
                    Ok(())
                } else {
                    Err(UpdateError::OutOfBounds)
                }
            }
            Accessor::_Hybrid {
                buffer: ram,
                flash,
                span,
            } => {
                let mut start_offset = offset as usize;
                let mut remainder = buffer.len();
                let end_offset = start_offset
                    .checked_add(remainder)
                    .ok_or(UpdateError::OutOfBounds)?;
                // Transfer data from the RAM portion of the image
                if start_offset < ram.len() {
                    let ram_end_offset = ram.len().min(end_offset);
                    // Transfer starts within the RAM part of this image.
                    let data = ram
                        .get((start_offset)..ram_end_offset)
                        .ok_or(UpdateError::OutOfBounds)?;
                    buffer.copy_from_slice(data);
                    remainder = remainder
                        .checked_sub(data.len())
                        .ok_or(UpdateError::OutOfBounds)?;
                    start_offset = ram_end_offset;
                }
                // Transfer data from the flash-backed portion of the image.
                if remainder > 0 {
                    let start = span
                        .stored
                        .start
                        .checked_add(start_offset as u32)
                        .ok_or(UpdateError::OutOfBounds)?;
                    let end = start
                        .checked_add(remainder as u32)
                        .ok_or(UpdateError::OutOfBounds)?;
                    if span.stored.contains(&start)
                        && (span.stored.start..=span.stored.end).contains(&end)
                    {
                        indirect_flash_read(flash, start, buffer)?;
                    } else {
                        return Err(UpdateError::OutOfBounds);
                    }
                }
                Ok(())
            }
        }
    }

    /// Get the rounded up length of an LPC55 image if present.
    pub fn padded_image_len(&self) -> Result<u32, UpdateError> {
        let vectors = match self.accessor {
            Accessor::Flash { .. } => {
                let buffer =
                    &mut [0u8; core::mem::size_of::<ImageVectorsLpc55>()];
                self.read_bytes(0u32, buffer.as_bytes_mut())?;
                ImageVectorsLpc55::read_from_prefix(&buffer[..])
                    .ok_or(UpdateError::OutOfBounds)
            }
            Accessor::Ram { buffer, .. } | Accessor::_Hybrid { buffer, .. } => {
                ImageVectorsLpc55::read_from_prefix(buffer)
                    .ok_or(UpdateError::OutOfBounds)
            }
        }?;
        let len = vectors.image_length().ok_or(UpdateError::BadLength)?;
        round_up_to_flash_page(len).ok_or(UpdateError::BadLength)
    }
}
