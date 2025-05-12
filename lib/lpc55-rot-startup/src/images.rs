// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use abi::ImageHeader;
use drv_lpc55_flash::{Flash, BYTES_PER_FLASH_PAGE};
use lpc55_pac::SYSCON;
use sha3::{Digest, Sha3_256};
use stage0_handoff::ImageError;
use zerocopy::IntoBytes;

const U32_SIZE: u32 = core::mem::size_of::<u32>() as u32;

// Corresponds to the ARM vector table on the NXP LPC55S69,
// NXP uses reserved vectors for image information.
// See UM11126 Table 177 for details.
#[repr(C)]
#[derive(Default, IntoBytes)]
pub struct ImageVectorsLpc55 {
    pub sp: u32,
    pub entry: u32,
    _unrelated_vectors_0: [u32; 6],
    pub nxp_image_length: u32,
    pub nxp_image_type: u32,
    pub nxp_offset_to_specific_header: u32,
    _unrelated_vectors_1: [u32; 2],
    pub nxp_image_execution_address: u32,
}

extern "C" {
    // __vector size is currently defined in the linker script as
    //
    // __vector_size = SIZEOF(.vector_table);
    //
    // which is a symbol whose value is the size of the vector table (i.e.
    // there is no actual space allocated). This is best represented as a zero
    // sized type which gets accessed by addr_of! as below.
    static __vector_size: [u8; 0];
}

// BYTES_PER_FLASH_PAGE is a usize so redefine the constant here to avoid having
// to do the u32 change everywhere
const PAGE_SIZE: u32 = BYTES_PER_FLASH_PAGE as u32;

pub struct FlashSlot {
    flash: Range<u32>,
    // The contiguous span of programmed flash pages starting at offset zero.
    // Note that any additional programmed pages after the first erased
    // page are not interesting for image sanity checks and are not included.
    initial_programmed_extent: Range<u32>,
    // Measurement over all pages including those that follow any erased page.
    fwid: [u8; 32],
}

impl FlashSlot {
    pub fn new(flash: &mut Flash<'_>, slot: Range<u32>) -> FlashSlot {
        // Find the extent of initial programmed pages while
        // hashing all programmed pages in the flash slot.
        let mut end: Option<u32> = None;
        let mut hash = Sha3_256::new();
        for page_start in slot.clone().step_by(BYTES_PER_FLASH_PAGE) {
            if flash.is_page_range_programmed(page_start, PAGE_SIZE) {
                let page = unsafe {
                    core::slice::from_raw_parts(
                        page_start as *const u8,
                        BYTES_PER_FLASH_PAGE,
                    )
                };
                hash.update(page);
            } else if end.is_none() {
                end = Some(page_start);
            }
        }
        let fwid = hash.finalize().into();
        let initial_programmed_extent = slot.start..end.unwrap_or(slot.end);
        FlashSlot {
            flash: slot,
            initial_programmed_extent,
            fwid,
        }
    }

    fn is_programmed(&self, addr: &u32) -> bool {
        self.initial_programmed_extent.contains(addr)
    }

    // True if the flash slot's span of contiguous programmed pages
    // starting at offset zero includes the given span.
    fn is_span_programmed(&self, start: u32, length: u32) -> bool {
        if let Some(end) = start.checked_add(length) {
            self.is_programmed(&start)
                && end <= self.initial_programmed_extent.end
        } else {
            false
        }
    }

    pub fn start(&self) -> u32 {
        self.flash.start
    }

    pub fn contains(&self, addr: &u32) -> bool {
        self.flash.contains(addr)
    }

    pub fn fwid(&self) -> [u8; 32] {
        self.fwid
    }
}

pub struct Image {
    // The boundaries of the actual image.
    span: Range<u32>,
    // The runtime address range
    run: Range<u32>,
}

impl Image {
    pub fn get_image_a(
        flash: &mut Flash<'_>,
        syscon: &SYSCON,
    ) -> (FlashSlot, Result<Image, ImageError>) {
        let slot = FlashSlot::new(flash, FLASH_A);
        let img = Image::new(&slot, FLASH_A, true, syscon);
        (slot, img)
    }

    pub fn get_image_b(
        flash: &mut Flash<'_>,
        syscon: &SYSCON,
    ) -> (FlashSlot, Result<Image, ImageError>) {
        let slot = FlashSlot::new(flash, FLASH_B);
        let img = Image::new(&slot, FLASH_B, true, syscon);
        (slot, img)
    }

    pub fn get_image_stage0(
        flash: &mut Flash<'_>,
        syscon: &SYSCON,
    ) -> (FlashSlot, Result<Image, ImageError>) {
        let slot = FlashSlot::new(flash, FLASH_STAGE0);
        let img = Image::new(&slot, FLASH_STAGE0, false, syscon);
        (slot, img)
    }

    pub fn get_image_stage0next(
        flash: &mut Flash<'_>,
        syscon: &SYSCON,
    ) -> (FlashSlot, Result<Image, ImageError>) {
        // Note that Stage0Next is not XIP until it gets copied to slot Stage0.
        let slot = FlashSlot::new(flash, FLASH_STAGE0NEXT);
        let img = Image::new(&slot, FLASH_STAGE0, false, syscon);
        (slot, img)
    }

    /// Before treating a span from a FlashSlot as an image:
    ///
    ///   - Find the image address boundaries.
    ///   - Sanity check the image.
    ///   - Check the image signature using the same ROM code as used at boot.
    ///
    /// If the image does not check out, the ImageError value should narrow
    /// down the problem.
    ///
    // Note: if partially programmed pages, i.e. one or more erased words in a
    // page, are a possibility that could be a problem with respect to
    // unexpected crashes or catching exfiltration attempts.
    fn new(
        slot: &FlashSlot,
        run: Range<u32>,
        header_required: bool,
        syscon: &SYSCON,
    ) -> Result<Image, ImageError> {
        // Make sure we can access the page where the vectors live.
        // Safety: Link time constants from our own image.
        let vector_size = core::ptr::addr_of!(__vector_size) as u32;
        if !slot.is_span_programmed(slot.start(), vector_size) {
            return Err(ImageError::FirstPageErased);
        }

        let vectors = slot.start() as *const u8 as *const ImageVectorsLpc55;
        // Safety: The address derives from link-time constants and
        // we have ensured that the first page of the flash slot is not erased.
        let vectors: &ImageVectorsLpc55 = unsafe { &*vectors };

        // Check that the entire image from flash.start to the end of
        // the header-declared signature block has been programmed.
        let image_length = vectors.nxp_image_length;
        let rounded_length = image_length
            .checked_next_multiple_of(PAGE_SIZE)
            .ok_or(ImageError::PartiallyProgrammed)?;
        if !slot.is_span_programmed(slot.start(), rounded_length) {
            // This also catches lengths that wrap around or exceed the slot.
            return Err(ImageError::PartiallyProgrammed);
        }

        // The ImageHeader page(s) need to be programmed for later calls to be
        // safe.
        if image_length
            < (vector_size + core::mem::size_of::<ImageHeader>() as u32)
        {
            if header_required {
                return Err(ImageError::HeaderNotProgrammed);
            } else {
                return Err(ImageError::BootloaderTooSmall);
            }
        }

        // TODO: Check that padding is 0xff.

        // After establishing that the entire image is programmed it's
        // ok to start using the Image methods.
        let img = Image {
            // Safety: Image length has been checked.
            span: slot.start()..(slot.start() + image_length),
            run,
        };

        img.validate(header_required).and_then(|_| {
            img.check_signature(syscon)?;
            Ok(img)
        })
    }

    fn get_img_start(&self) -> u32 {
        self.span.start
    }

    // Return a pointer to the NXP vector table in flash.
    // N.B: Before calling, check that the first flash page is programmed.
    fn get_vectors(&self) -> &ImageVectorsLpc55 {
        let vectors = self.span.start as *const u8 as *const ImageVectorsLpc55;
        // Safety: The address derives from a link-time constant and
        // the caller has ensured that the first page of flash is not
        // erased.
        unsafe { &*vectors }
    }

    fn get_reset_vector(&self) -> u32 {
        self.get_vectors().entry
    }

    fn get_image_type(&self) -> u32 {
        self.get_vectors().nxp_image_type
    }

    // Get a pointer to where the ImageHeader should be.
    // Note that it may not be present if the image
    // is corrupted or is a bootloader.
    fn get_header_ptr(&self) -> *const ImageHeader {
        let vector_size = core::ptr::addr_of!(__vector_size) as u32;
        (self.get_img_start() + vector_size) as *const ImageHeader
    }

    fn get_imageheader(&self) -> Result<&ImageHeader, ImageError> {
        // Check Hubris header.
        // Note that bootloaders without Hubris headers have been released.
        let header_ptr = self.get_header_ptr();

        // Safety: We've validated the header location is programmed so this
        // will not trigger a fault.
        // The values used are all link-time constants.
        let header = unsafe { &*header_ptr };
        if header.magic != abi::HEADER_MAGIC {
            return Err(ImageError::BadMagic);
        }
        Ok(header)
    }

    fn get_imageheader_total_image_len(&self) -> Result<u32, ImageError> {
        Ok(self.get_imageheader()?.total_image_len)
    }

    /// Test an image for viability.
    fn validate(&self, header_required: bool) -> Result<(), ImageError> {
        // The signature validation routine could be called now.
        // Any additional checks should be moot based on the signing
        // procedure only signing "good" images.
        //
        // However, the price of flashing a bad bootloader is high and
        // the criteria for signing images evolves over time. So, as
        // long as we can afford the space and time, perform extra checks
        // to aid diagnosis of bad images and to protect the system.
        //
        // There is also the concern that the ROM signature check routine
        // might not fully protect itself from bad input.
        //
        // Consider deleting any of the following tests once there
        // is high confidence that non-conforming signed images are
        // no longer a threat.

        // Bootloaders without Hubris headers have been released.
        // So, check ImageHeader carefully.
        if header_required {
            let len = self.get_imageheader_total_image_len()?;
            if (len % U32_SIZE) != 0 {
                return Err(ImageError::UnalignedLength);
            }
            match self.span.start.checked_add(len) {
                Some(end) => {
                    if !self.span.contains(&end) {
                        return Err(ImageError::HeaderImageSize);
                    }
                }
                None => return Err(ImageError::HeaderImageSize),
            };
        }

        // Because of our past experience with the implementation quality of the
        // ROM, let's do some basic checks before handing it a blob to inspect,
        // shall we?

        const MASK_WITHOUT_28: u32 = !(1 << 28);
        let reset_vector = MASK_WITHOUT_28 & self.get_reset_vector();

        // Verify that the reset vector is a valid Thumb-2 function pointer.
        if reset_vector & 1 == 0 {
            // This'll cause an immediate usage fault. Reject it.
            return Err(ImageError::ResetVectorNotThumb2);
        }

        // Ensure that the reset vector is within the runtime address range.
        let runtime = self.run.start..(self.run.start + self.span.len() as u32);
        if !runtime.contains(&reset_vector) {
            return Err(ImageError::ResetVector);
        }

        // The image type is at offset 0x24. We only allow type 4.
        //   - 0x0000 Normal image for unsecure boot
        //   - 0x0001 Plain signed Image
        //   - 0x0002 Plain CRC Image, CRC at offset 0x28
        //   - 0x0004 Plain signed XIP Image
        //   - 0x0005 Plain CRC XIP Image, CRC at offset 0x28
        //   - 0x8001 Signed plain Image with KeyStore Included
        if self.get_image_type() != 4 {
            return Err(ImageError::UnsupportedType);
        }

        Ok(())
    }

    // Assuming a well-formed image, get the result of the ROM signature check
    // routine.
    fn check_signature(&self, syscon: &SYSCON) -> Result<(), ImageError> {
        syscon
            .presetctrl2
            .modify(|_, w| w.hash_aes_rst().released());

        let authorized = unsafe {
            lpc55_romapi::authenticate_image(self.span.start).is_ok()
        };

        syscon
            .presetctrl2
            .modify(|_, w| w.hash_aes_rst().asserted());

        if authorized {
            Ok(())
        } else {
            Err(ImageError::Signature)
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/config.rs"));
