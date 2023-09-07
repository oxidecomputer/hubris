// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use abi::ImageHeader;
use drv_lpc55_flash::{Flash, BYTES_PER_FLASH_PAGE};
use sha3::{Digest, Sha3_256};
use stage0_handoff::{ImageVersion, RotImageDetails};
use unwrap_lite::UnwrapLite;

pub fn get_image_b(flash: &mut Flash<'_>) -> Option<Image> {
    let img = Image::new(flash, FLASH_B);
    if img.validate() {
        Some(img)
    } else {
        None
    }
}

pub fn get_image_a(flash: &mut Flash<'_>) -> Option<Image> {
    let img = Image::new(flash, FLASH_A);
    if img.validate() {
        Some(img)
    } else {
        None
    }
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
    // The initial span of programmed flash pages.
    programmed: Range<u32>,
    // The number of pages that contain no erased flash words.
    // _count: usize,
    // Measurement over the `count`ed pages.
    fwid: [u8; 32],
}

impl Image {
    // Before doing any other work with a chunk of flash memory:
    //
    //   - Define the address boundaries.
    //   - Determine the bounds of the initial programmed extent.
    //   - Count the total number of fully programmed pages.
    //   - Measure all programmed pages including those outside of the
    //     initial programmed extent.
    //
    // Note: if partially programmed pages is a possibility then that could be
    // a problem with respect to catching exfiltration attempts.
    pub fn new(dev: &mut Flash, flash: Range<u32>) -> Image {
        // let mut _count = 0;
        let mut end: Option<u32> = None;
        let mut hash = Sha3_256::new();
        for start in flash.clone().step_by(BYTES_PER_FLASH_PAGE) {
            if dev.is_page_range_programmed(start, PAGE_SIZE) {
                // _count += 1;
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
            programmed,
            // _count,
            fwid,
        }
    }

    pub fn is_programmed(&self, addr: &u32) -> bool {
        return self.programmed.contains(addr);
    }

    pub fn is_span_programmed(&self, start: u32, length: u32) -> bool {
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
}

pub fn image_details(img: Image) -> RotImageDetails {
    RotImageDetails {
        digest: img.fwid,
        version: img.get_image_version(),
    }
}

impl Image {
    fn get_img_start(&self) -> u32 {
        self.flash.start
    }

    fn get_img_size(&self) -> Option<usize> {
        usize::try_from((unsafe { &*self.get_header() }).total_image_len).ok()
    }

    fn get_header(&self) -> *const ImageHeader {
        // SAFETY: This generated by the linker script which we trust
        // Note that this is generated from _this_ image's linker script
        // as opposed to the _image_ linker script but those two _must_
        // be the same value!
        let vector_size = unsafe { core::ptr::addr_of!(__vector_size) as u32 };
        (self.get_img_start() + vector_size) as *const ImageHeader
    }

    /// Make sure all of the image flash is programmed
    fn validate(&self) -> bool {
        let img_start = self.get_img_start();

        // Start by making sure we can access the page where the vectors live
        if !self.is_span_programmed(self.flash.start, PAGE_SIZE) {
            return false;
        }

        let header_ptr = self.get_header();

        // Next validate the header location is programmed
        if !self.is_span_programmed(header_ptr as u32, PAGE_SIZE) {
            return false;
        }

        // SAFETY: We've validated the header location is programmed so this
        // will not trigger a fault. This is generated from our build scripts
        // which we trust.
        let header = unsafe { &*header_ptr };

        // Does this look like a header?
        if header.magic != abi::HEADER_MAGIC {
            return false;
        }

        let total_len = match header.total_image_len.checked_add(PAGE_SIZE - 1)
        {
            Some(s) => s & !(PAGE_SIZE - 1),
            None => return false,
        };

        // Last step is to make sure the entire range is programmed
        if !self.is_span_programmed(img_start, total_len) {
    }

    pub fn get_image_version(&self) -> ImageVersion {
        // SAFETY: We checked this previously
        let header = unsafe { &*self.get_header() };

        ImageVersion {
            epoch: header.epoch,
            version: header.version,
        }
    }

    fn pointer_range(&self) -> core::ops::Range<*const u8> {
        let img_ptr = self.get_img_start() as *const u8;
        // The MPU requires 32 byte alignment and so the compiler pads the
        // image accordingly. The length field from the image header does not
        // (and should not) account for this padding so we must do that here.
        let img_size = (self.get_img_size().unwrap_lite() + 31) & !31;

        // Safety: this is unsafe because the pointer addition could overflow.
        // If that happens, we'll produce an empty range or crash with a panic.
        // We do not dereference these here pointers.
        img_ptr..unsafe { img_ptr.add(img_size) }
    }

    pub fn contains(&self, address: *const u8) -> bool {
        self.pointer_range().contains(&address)
    }
}

include!(concat!(env!("OUT_DIR"), "/config.rs"));
