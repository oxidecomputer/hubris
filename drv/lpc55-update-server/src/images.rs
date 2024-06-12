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
use drv_lpc55_flash::BYTES_PER_FLASH_PAGE;
use drv_lpc55_update_api::{RawCabooseError, RotComponent, SlotId};
use drv_update_api::UpdateError;
use zerocopy::{AsBytes, FromBytes};

// Our layout of flash banks on the LPC55.
// Addresses are from the linker.
// The bootloader (`bootleby`) resides at __STAGE0_BASE and
// only cares about IMAGE_A and IMAGE_B.
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

// An image may have an ImageHeader located after the
// LPC55's mixed header/vector table.
pub const IMAGE_HEADER_OFFSET: u32 = 0x130;

/// Address ranges that may contain an image during storage and active use.
/// `store` and `exec` ranges are the same except for `stage0next`.
pub struct FlashRange {
    pub store: Range<u32>,
    pub exec: Range<u32>,
}

/// Get the flash storage address range and flash execution address range.
pub fn flash_range(component: RotComponent, slot: SlotId) -> FlashRange {
    // Safety: Linker defined symbols are trusted.
    unsafe {
        match (component, slot) {
            (RotComponent::Hubris, SlotId::A) => FlashRange {
                store: __IMAGE_A_BASE.as_ptr() as u32
                    ..__IMAGE_A_END.as_ptr() as u32,
                exec: __IMAGE_A_BASE.as_ptr() as u32
                    ..__IMAGE_A_END.as_ptr() as u32,
            },
            (RotComponent::Hubris, SlotId::B) => FlashRange {
                store: __IMAGE_B_BASE.as_ptr() as u32
                    ..__IMAGE_B_END.as_ptr() as u32,
                exec: __IMAGE_B_BASE.as_ptr() as u32
                    ..__IMAGE_B_END.as_ptr() as u32,
            },
            (RotComponent::Stage0, SlotId::A) => FlashRange {
                store: __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
                exec: __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
            },
            (RotComponent::Stage0, SlotId::B) => FlashRange {
                store: __IMAGE_STAGE0NEXT_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0NEXT_END.as_ptr() as u32,
                exec: __IMAGE_STAGE0_BASE.as_ptr() as u32
                    ..__IMAGE_STAGE0_END.as_ptr() as u32,
            },
        }
    }
}

/// Does (component, slot) refer to the currently running Hubris image?
pub fn same_image(component: RotComponent, slot: SlotId) -> bool {
    // Safety: We are trusting the linker.
    flash_range(component, slot).store.start
        == unsafe { &__this_image } as *const _ as u32
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
    pub fn padded_image_range(&self, exec: &Range<u32>) -> Option<Range<u32>> {
        let image_start = exec.start;
        let image_end = exec.start.saturating_add(self.padded_image_len()?);
        // ImageHeader is optional. Assuming code cannot be earlier than
        // IMAGE_HEADER_OFFSET.
        let initial_pc_start = image_start.saturating_add(IMAGE_HEADER_OFFSET);
        let initial_pc_end =
            image_start.saturating_add(self.nxp_offset_to_specific_header);
        let pc = exec.start.saturating_add(self.normalized_initial_pc());

        if !exec.contains(&image_end)
            || !exec.contains(&initial_pc_start)
            || !exec.contains(&initial_pc_end)
            || !(initial_pc_start..initial_pc_end).contains(&pc)
        {
            // One of these did not fit in the destination flash bank or
            // exeecutable area of the image in that bank.
            return None;
        }
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

    if header_access.read_bytes(0, vectors.as_bytes_mut()).is_err()
        || header_access
            .read_bytes(IMAGE_HEADER_OFFSET, header.as_bytes_mut())
            .is_err()
    {
        // can't read block0
        return Err(UpdateError::InvalidHeaderBlock);
    }

    if !vectors.is_image_type_signed_xip()
        || vectors.nxp_offset_to_specific_header >= vectors.nxp_image_length
    {
        // Not a signed XIP image or no signature block.
        // If we figure out a reasonable minimum size for the signature block
        // we should test for that.
        return Err(UpdateError::InvalidHeaderBlock);
    }

    // We don't rely on the ImageHeader, but if it is there, it needs to be valid.
    // Note that ImageHeader.epoch is needed for pre-flash-erase tests for
    // rollback protection.
    // TODO: Improve estimate of where the first executable instruction can be.
    let code_offset = if header.magic == HEADER_MAGIC {
        if header.total_image_len != vectors.nxp_offset_to_specific_header {
            // ImageHeader disagrees with LPC55 vectors.
            return Err(UpdateError::InvalidHeaderBlock);
        }
        IMAGE_HEADER_OFFSET + (core::mem::size_of::<ImageHeader>() as u32)
    } else {
        IMAGE_HEADER_OFFSET
    };

    if vectors.nxp_image_length as usize > header_access.exec().len() {
        // Image extends outside of flash bank.
        return Err(UpdateError::InvalidHeaderBlock);
    }

    let caboose_end = header_access
        .exec()
        .start
        .saturating_add(vectors.nxp_offset_to_specific_header);
    let text_start = header_access.exec().start.saturating_add(code_offset);
    if !(text_start..caboose_end).contains(&vectors.normalized_initial_pc()) {
        return Err(UpdateError::InvalidHeaderBlock);
    }

    Ok(vectors.nxp_offset_to_specific_header)
}

/// Get the range of the coboose contained within an image if it exists.
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
    let caboose_size_offset: u32 = image_end_offset.saturating_sub(U32_SIZE);
    let caboose_size = image
        .read_word(caboose_size_offset)
        .map_err(|_| RawCabooseError::ReadFailed)?;

    // Security considerations:
    // If there is no caboose, then we may be indexing 0xff padding, or
    // code/data and padding, or just code/data, and interpreting that as
    // the caboose size. After an alignment and size check, some values
    // could still get through. The pathological case would be code/data
    // that indexes the CABOOSE_MAGIC constant in the code here that is
    // testing for the caboose. The parts of the image that could then
    // be accessed are already openly available. There doesn't seem
    // to be an opportunity for any denial of service.
    let caboose_magic_offset = image_end_offset.saturating_sub(caboose_size);
    if ((caboose_magic_offset % U32_SIZE) != 0)
        || !(IMAGE_HEADER_OFFSET..caboose_size_offset)
            .contains(&caboose_magic_offset)
    {
        return Err(RawCabooseError::MissingCaboose);
    }

    let caboose_magic = image
        .read_word(caboose_magic_offset as u32)
        .map_err(|_| RawCabooseError::MissingCaboose)?;

    if caboose_magic == CABOOSE_MAGIC {
        let caboose_range = ((caboose_magic_offset as u32) + U32_SIZE)
            ..(caboose_size_offset as u32);
        Ok(caboose_range)
    } else {
        Err(RawCabooseError::MissingCaboose)
    }
}

// Accessor keeps the implementation details of ImageAccess private
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
    fn exec(&self) -> &Range<u32> {
        match self {
            Accessor::Flash { flash: _, span }
            | Accessor::Ram { buffer: _, span }
            | Accessor::_Hybrid {
                buffer: _,
                flash: _,
                span,
            } => &span.exec,
        }
    }
}

/// The update_server needs to deal with images that are:
///   - Entirely in flash
///   - Header block in RAM with the remainder unavailable
///   - Header block in RAM with the remainder in flash.
///   - Entire image in RAM (cached Stage0)
/// Calls to methods use image relative addresses.
/// ImageAccess::Flash{_, span} gives the physical flash addresses
/// being accessed.
///
/// Any address here is the "storage" address which for stage0next
/// and any image held temporarily in RAM is different than the
/// execution address.
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
        assert!((buffer.len() % BYTES_PER_FLASH_PAGE) == 0);
        assert!(((buffer.as_ptr() as u32) % U32_SIZE) == 0);
        let span = flash_range(component, slot);
        ImageAccess {
            accessor: Accessor::_Hybrid {
                flash,
                buffer,
                span,
            },
        }
    }

    fn exec(&self) -> &Range<u32> {
        self.accessor.exec()
    }

    /// True if the u32 at offset is contained within the image.
    pub fn is_addressable(&self, offset: u32) -> bool {
        let len = match &self.accessor {
            Accessor::Flash { flash: _, span }
            | Accessor::_Hybrid {
                buffer: _,
                flash: _,
                span,
            }
            | Accessor::Ram { buffer: _, span } => {
                span.store.end.saturating_sub(span.store.start)
            }
        };
        offset < len && offset.saturating_add(U32_SIZE) <= len
    }

    // Fetch a u32 from an image.
    pub fn read_word(&self, offset: u32) -> Result<u32, UpdateError> {
        if !self.is_addressable(offset) {
            return Err(UpdateError::OutOfBounds);
        }
        match &self.accessor {
            Accessor::Flash { flash, span } => {
                let addr = span.store.start.saturating_add(offset);
                let mut word = 0u32;
                indirect_flash_read(flash, addr, word.as_bytes_mut())?;
                Ok(word)
            }
            Accessor::Ram { buffer, span: _ } => {
                match buffer
                    .get(offset as usize..(offset as usize + SIZEOF_U32))
                    .and_then(u32::read_from)
                {
                    Some(word) => Ok(word),
                    None => Err(UpdateError::OutOfBounds),
                }
            }
            Accessor::_Hybrid {
                buffer,
                flash,
                span,
            } => {
                if offset < buffer.len() as u32 {
                    match buffer
                        .get(offset as usize..(offset as usize + SIZEOF_U32))
                        .and_then(u32::read_from)
                    {
                        Some(word) => Ok(word),
                        // Note: The case of a transfer spanning the RAM/Flash
                        // boundary would need to be unaligned given that the
                        // RAM portion must be whole u32-aligned pages.
                        None => Err(UpdateError::OutOfBounds),
                    }
                } else {
                    let addr = span.store.start.saturating_add(offset);
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
                let start = offset.saturating_add(span.store.start);
                let end =
                    offset.saturating_add(span.store.start).saturating_add(len);
                if span.store.contains(&start)
                    && (span.store.start..=span.store.end).contains(&end)
                {
                    Ok(indirect_flash_read(flash, start, buffer)?)
                } else {
                    Err(UpdateError::OutOfBounds)
                }
            }
            Accessor::Ram {
                buffer: src,
                span: _,
            } => {
                if let Some(data) = src.get(
                    (offset as usize)..(offset.saturating_add(len) as usize),
                ) {
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
                let mut offset = offset as usize;
                let mut remainder = buffer.len();
                // Transfer data from the RAM portion of the image
                if offset < ram.len() {
                    let ram_end_offset = ram.len().min(offset + remainder);
                    // Transfer starts within the RAM part of this image.
                    let data = ram
                        .get((offset)..ram_end_offset)
                        .ok_or(UpdateError::OutOfBounds)?;
                    buffer.copy_from_slice(data);
                    remainder -= data.len();
                    offset = ram_end_offset;
                }
                // Transfer data from the flash-backed portion of the image.
                if remainder > 0 {
                    let start =
                        offset.saturating_add(span.store.start as usize);
                    let end = start.saturating_add(remainder);
                    if span.store.contains(&(start as u32))
                        && (span.store.start..=span.store.end)
                            .contains(&(end as u32))
                    {
                        indirect_flash_read(flash, start as u32, buffer)?;
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
            Accessor::Ram { buffer, span: _ }
            | Accessor::_Hybrid {
                buffer,
                flash: _,
                span: _,
            } => ImageVectorsLpc55::read_from_prefix(buffer)
                .ok_or(UpdateError::OutOfBounds),
        }?;
        let len = vectors.image_length().ok_or(UpdateError::BadLength)?;
        round_up_to_flash_page(len).ok_or(UpdateError::BadLength)
    }
}
