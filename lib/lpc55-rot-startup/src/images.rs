// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use abi::{ImageHeader, ImageVectors};
use drv_lpc55_flash::{Flash, BYTES_PER_FLASH_PAGE};
use sha3::{Digest, Sha3_256};
use stage0_handoff::{ImageError, ImageVersion, RotImageDetails};
use unwrap_lite::UnwrapLite;

const U32_SIZE: u32 = core::mem::size_of::<u32>() as u32;

macro_rules! set_bit {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() | $mask) })
    };
}

macro_rules! clear_bit {
    ($reg:expr, $mask:expr) => {
        $reg.modify(|r, w| unsafe { w.bits(r.bits() & !$mask) })
    };
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

pub struct Image {
    // The boundaries of the flash slot.
    flash: Range<u32>,
    // The runtime address range (stage0next flash != run).
    run: Range<u32>,
    // The contiguous span of programmed flash pages starting at offset zero.
    // Note that any additional programmed pages after the first erased
    // page are not interesting for image sanity checks and are not included.
    programmed: Range<u32>,
    // Measurement over all pages includeing those that follow any erased page.
    fwid: [u8; 32],
    status: Result<(), ImageError>,
}

impl Image {
    pub fn get_image_b(flash: &mut Flash) -> Image {
        let mut img = Image::new(flash, FLASH_B, FLASH_B);
        img.status = img.validate(true);
        img
    }

    pub fn get_image_a(flash: &mut Flash) -> Image {
        let mut img = Image::new(flash, FLASH_A, FLASH_A);
        img.status = img.validate(true);
        img
    }

    pub fn get_image_stage0(flash: &mut Flash) -> Image {
        let mut img = Image::new(flash, FLASH_STAGE0, FLASH_STAGE0);
        img.status = img.validate(false);
        img
    }

    pub fn get_image_stage0next(flash: &mut Flash) -> Image {
        let mut img = Image::new(flash, FLASH_STAGE0_NEXT, FLASH_STAGE0);
        img.status = img.validate(false);
        img
    }

    pub fn image_details(&self) -> RotImageDetails {
        RotImageDetails {
            digest: self.fwid,
            version: self.get_image_version(),
            status: self.status,
        }
    }

    pub fn slot_contains(&self, addr: u32) -> bool {
        self.flash.contains(&addr)
    }

    // Before doing any other work with a chunk of flash memory:
    //
    //   - Define the address boundaries.
    //   - Determine the bounds of the initial programmed extent.
    //   - Measure all programmed pages including those outside of the
    //     initial programmed extent.
    //
    // Note: if partially programmed pages are a possibility then that could be
    // a problem with respect to catching exfiltration attempts.
    //
    // Later functions are safe to access flash if they are relying on
    // self.programmed.contains() directly or indirectly.
    fn new(dev: &mut Flash, flash: Range<u32>, run: Range<u32>) -> Image {
        let mut end: Option<u32> = None;
        let mut hash = Sha3_256::new();
        for start in flash.clone().step_by(BYTES_PER_FLASH_PAGE) {
            if dev.is_page_range_programmed(start, PAGE_SIZE) {
                let page = unsafe {
                    core::slice::from_raw_parts(
                        start as *const u8,
                        BYTES_PER_FLASH_PAGE,
                    )
                };
                hash.update(page);
            } else if end.is_none() {
                end = Some(start);
            }
        }
        let fwid = hash.finalize().try_into().unwrap_lite();
        let programmed = Range {
            start: flash.start,
            end: end.unwrap_or(flash.end),
        };
        Image {
            flash,
            run,
            programmed,
            fwid,
            status: Err(ImageError::Unchecked),
        }
    }

    fn is_programmed(&self, addr: &u32) -> bool {
        return self.programmed.contains(addr);
    }

    // True if the flash slot's span of contiguous programmed pages
    // starting at offset zero includes the given span.
    fn is_span_programmed(&self, start: u32, length: u32) -> bool {
        if let Some(end) = start.checked_add(length) {
            if !self.is_programmed(&start) || !self.is_programmed(&end) {
                false
            } else {
                true
            }
        } else {
            false
        }
    }

    fn get_img_start(&self) -> u32 {
        self.flash.start
    }

    // Return a pointer to the NXP vector table in flash.
    // N.B: Before calling, check that the first flash page is programmed.
    fn get_nxp_vectors(&self) -> &ImageVectors {
        let vectors = self.flash.start as *const u8 as *const ImageVectors;
        // SAFETY: The address derives from a link-time constant and
        // the caller has ensured that the first page of flash is not
        // erased.
        unsafe { &*vectors }
    }

    fn get_nxp_image_length(&self) -> u32 {
        self.get_nxp_vectors().nxp_image_length
    }

    fn get_reset_vector(&self) -> u32 {
        self.get_nxp_vectors().entry
    }

    fn get_image_type(&self) -> u32 {
        self.get_nxp_vectors().nxp_image_type
    }

    fn get_type_specific_header(&self) -> u32 {
        self.get_nxp_vectors().nxp_offset_to_specific_header
    }

    // Get a pointer to where the ImageHeader should be.
    // Note that it may not be present if the image
    // is corrupted or is a bootloader.
    fn get_header_ptr(&self) -> *const ImageHeader {
        // SAFETY: This is generated by the linker script which we trust
        // Note that this is generated from _this_ image's linker script
        // as opposed to the _image_ linker script but those two _must_
        // be the same value!
        let vector_size = unsafe { core::ptr::addr_of!(__vector_size) as u32 };
        (self.get_img_start() + vector_size) as *const ImageHeader
    }

    fn is_imageheader_present(
        &self,
        header_required: bool,
    ) -> Result<bool, ImageError> {
        // Check Hubris header.
        // Note that bootloaders without Hubris headers have been released.
        let header_ptr = self.get_header_ptr();

        // Even headerless bootloaders are long enough that
        // it is an error if this area is not programmed.
        if !self.is_span_programmed(
            header_ptr as u32,
            core::mem::size_of::<ImageHeader>() as u32,
        ) {
            if header_required {
                return Err(ImageError::HeaderNotProgrammed);
            } else {
                return Err(ImageError::Short);
            }
        }

        // SAFETY: We've validated the header location is programmed so this
        // will not trigger a fault.
        // The values used are all link-time constants.
        let header = unsafe { &*header_ptr };
        if header.magic != abi::HEADER_MAGIC {
            if header_required {
                return Err(ImageError::BadMagic);
            }
            Ok(false)
        } else {
            Ok(true)
        }
    }

    fn get_imageheader_total_image_len(&self) -> u32 {
        let header_ptr = self.get_header_ptr();
        // SAFETY: The caller must have established that a header is present.
        let header = unsafe { &*header_ptr };
        header.total_image_len
    }

    /// Test an image for viability.
    fn validate(&self, header_required: bool) -> Result<(), ImageError> {
        // Start by making sure we can access the page where the vectors live
        if !self.is_span_programmed(self.flash.start, PAGE_SIZE) {
            return Err(ImageError::FirstPageErased);
        }

        // Check that the entire image from flash.start to the end of
        // the signature block has been programmed.
        let image_length = self.get_nxp_image_length();
        let end = if let Some(end) = self.flash.start.checked_add(image_length)
        {
            // The programmed image is rounded to a page boundary with padding
            // of 0xff.
            // Later checks ensure that interesting offsets fall within
            // the image proper.
            // Check rounded limit here and save image limit.
            match end
                .checked_add(PAGE_SIZE - 1)
                .map(|sum| sum & !(PAGE_SIZE - 1))
            {
                Some(rounded_end) => {
                    if !self.programmed.contains(&rounded_end) {
                        return Err(ImageError::PartiallyProgrammed);
                    }
                } // TODO: Check that padding is 0xff.
                None => return Err(ImageError::InvalidLength),
            }
            end
        } else {
            // A offset was found that lies outside the flash slot.
            return Err(ImageError::InvalidLength);
        };

        let image_span = Range::<u32> {
            start: self.flash.start,
            end,
        };

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
        if self.is_imageheader_present(header_required)? {
            let len = self.get_imageheader_total_image_len();
            if (len % U32_SIZE) != 0 {
                return Err(ImageError::UnalignedLength);
            }
            match self.flash.start.checked_add(len) {
                Some(end) => {
                    if !image_span.contains(&end) {
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
        if !(self.get_executable_image_range()?.contains(&reset_vector)) {
            return Err(ImageError::ResetVector);
        }

        self.check_signature()
    }

    // Any vector address must fall within the executable
    // code portion of an image. At this point, we don't know
    // what is code vs data, but we can exclude the vector
    // table, the ImageHeader, the Caboose, and the signature block.
    // Note that this range is different from the flash range for
    // `stage0next`.
    fn get_executable_image_range(&self) -> Result<Range<u32>, ImageError> {
        let end_offset = match self.get_image_type() {
            2 | 5 => self.get_nxp_image_length(),
            1 | 4 | 0x8001 => self.get_type_specific_header(),
            _ => return Err(ImageError::UnsupportedType),
        };

        // TODO: Find the caboose if present and reduce end_offset.

        // SAFETY: Link time constants known to be safe.
        let mut begin_offset =
            unsafe { core::ptr::addr_of!(__vector_size) as u32 };
        if self.is_imageheader_present(false).is_ok() {
            // SAFETY: Link time constants known to be safe.
            begin_offset += core::mem::size_of::<ImageHeader>() as u32;
        }
        if let Some(end) = self.run.end.checked_add(end_offset) {
            // SAFETY: Link time constants known to be safe.
            Ok(self.run.start + begin_offset..end)
        } else {
            Err(ImageError::InvalidLength)
        }
    }

    // Assuming a well-formed image, get the result of the ROM signature check
    // routine.
    fn check_signature(&self) -> Result<(), ImageError> {
        // The following code is adapted to fit here from bootleby.

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

        let syscon = unsafe { &*lpc55_pac::SYSCON::ptr() };
        // Time to check the signatures!

        const HASHAES: u32 = 32 + 32 + 18;
        const PMASK: u32 = 1 << (HASHAES % 32);
        const REG_NUM: u32 = HASHAES / 32; // XXX must be 0, 1, or 2
        match REG_NUM {
            0 => clear_bit!(syscon.presetctrl0, PMASK),
            1 => clear_bit!(syscon.presetctrl1, PMASK),
            2 => clear_bit!(syscon.presetctrl2, PMASK),
            _ => panic!(),
        }

        let authorized = unsafe {
            lpc55_romapi::authenticate_image(self.flash.start).is_ok()
        };
        // let authorized = true;
        // enter reset
        match REG_NUM {
            0 => set_bit!(syscon.presetctrl0, PMASK),
            1 => set_bit!(syscon.presetctrl1, PMASK),
            2 => set_bit!(syscon.presetctrl2, PMASK),
            _ => panic!(),
        }

        if authorized {
            Ok(())
        } else {
            Err(ImageError::Signature)
        }
    }

    // Get the epoch and version from a flash slot or (0,0) if there
    // is no valid image.
    fn get_image_version(&self) -> ImageVersion {
        let header_ptr = self.get_header_ptr();
        if self.is_span_programmed(
            header_ptr as u32,
            core::mem::size_of::<ImageHeader>() as u32,
        ) {
            // SAFETY: header page is available.
            let header = unsafe { &*header_ptr };

            if header.magic == abi::HEADER_MAGIC {
                return ImageVersion {
                    epoch: header.epoch,
                    version: header.version,
                };
            }
        }
        // Default for erased slots and images without ImageHeaders is 0,0
        ImageVersion {
            epoch: 0,
            version: 0,
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/config.rs"));
