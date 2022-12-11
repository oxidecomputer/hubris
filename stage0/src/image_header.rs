// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use abi::{ImageHeader, ImageVectors};
use lpc55_romapi::FLASH_PAGE_SIZE;

use lpc55_pac::Peripherals;
use sha3::{Digest, Sha3_256};
use stage0_handoff::{
    Handoff, ImageVersion, RotImageDetails, RotSlot, RotUpdateDetails,
};
use unwrap_lite::UnwrapLite;

extern "C" {
    static IMAGEA: abi::ImageVectors;
    static IMAGEB: abi::ImageVectors;
    // __vector size is currently defined in the linker script as
    //
    // __vector_size = SIZEOF(.vector_table);
    //
    // which is a symbol whose value is the size of the vector table (i.e.
    // there is no actual space allocated). This is best represented as a zero
    // sized type which gets accessed by addr_of! as below. The issue here is
    // that () is not a valid C type and this is a C extern block. We don't
    // actually care about compatibility with C here though so we can safely
    // allow an improper ctype here.
    #[allow(improper_ctypes)]
    static __vector_size: ();
}

pub struct Image(&'static ImageVectors);

// FLASH_PAGE_SIZE is a usize so redefine the constant here to avoid having
// to do the u32 change everywhere
const PAGE_SIZE: u32 = FLASH_PAGE_SIZE as u32;

// Implicit in this design is that all functions on Image are considered safe.
// We ensure this by only returning an Image through this interface after
// verifying all parts of it are valid.
//
// It would technically be possible to create an instance of Image with an
// invalid set of ImageVectors but that would require going far outside the
// bounds of the expected design.

// Safety: These accesses are unsafe because `IMAGEA` and `IMAGEB`
// are coming from an extern, and might violate alignment rules or even be
// modified externally and subject to data races. In our case
// we have to assume that neither of these is true, since it's
// being furnished by our linker script, which we trust.

pub fn get_image_b() -> Option<Image> {
    let imageb = unsafe { &IMAGEB };

    let img = Image(imageb);

    if img.validate() {
        Some(img)
    } else {
        None
    }
}

pub fn get_image_a() -> Option<Image> {
    let imagea = unsafe { &IMAGEA };

    let img = Image(imagea);

    if img.validate() {
        Some(img)
    } else {
        None
    }
}

impl Image {
    fn get_img_start(&self) -> u32 {
        self.0 as *const ImageVectors as u32
    }

    //  #[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
    fn get_img_size(&self) -> Option<usize> {
        usize::try_from((unsafe { &*self.get_header() }).total_image_len).ok()
    }

    //    #[cfg(any(feature = "dice-mfg", feature = "dice-self"))]
    pub fn as_bytes(&self) -> &[u8] {
        let img_ptr = self.get_img_start() as *const u8;
        let img_size = self.get_img_size().unwrap_lite();
        unsafe { core::slice::from_raw_parts(img_ptr, img_size) }
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
        let valid = lpc55_romapi::validate_programmed(img_start, PAGE_SIZE);

        if !valid {
            return false;
        }

        let header_ptr = self.get_header();

        // Next validate the header location is programmed
        let valid =
            lpc55_romapi::validate_programmed(header_ptr as u32, PAGE_SIZE);

        if !valid {
            return false;
        }

        // SAFETY: We've validated the header location is programmed so this
        // will not trigger a fault. This is generated from our build scripts
        // which we trust.
        let header = unsafe { &*header_ptr };

        // Next make sure the marked image length is programmed
        let valid = lpc55_romapi::validate_programmed(
            img_start,
            (header.total_image_len + (PAGE_SIZE - 1)) & !(PAGE_SIZE - 1),
        );

        if !valid {
            return false;
        }

        // Does this look correct?
        if header.magic != abi::HEADER_MAGIC {
            return false;
        }

        return true;
    }

    pub fn get_vectors(&self) -> u32 {
        self.get_img_start()
    }

    pub fn get_pc(&self) -> u32 {
        self.0.entry
    }

    pub fn get_sp(&self) -> u32 {
        self.0.sp
    }

    pub fn get_version(&self) -> u32 {
        // SAFETY: We checked this previously
        let header = unsafe { &*self.get_header() };

        header.version
    }

    // TODO(AJS): Replace get_version with this?
    pub fn get_image_version(&self) -> ImageVersion {
        // SAFETY: We checked this previously
        let header = unsafe { &*self.get_header() };

        ImageVersion {
            epoch: header.epoch,
            version: header.version,
        }
    }

    #[cfg(feature = "tz_support")]
    pub fn get_sau_entry<'a>(&self, i: usize) -> Option<&'a abi::SAUEntry> {
        // SAFETY: We checked this previously
        let header = unsafe { &*self.get_header() };

        header.sau_entries.get(i)
    }
}

pub fn select_image_to_boot() -> (Image, RotSlot) {
    let (imagea, imageb) = (get_image_a(), get_image_b());

    // Image selection is very simple at the moment
    // Future work: check persistent state and epochs
    match (imagea, imageb) {
        (None, None) => panic!(),
        (Some(a), None) => (a, RotSlot::A),
        (None, Some(b)) => (b, RotSlot::B),
        (Some(a), Some(b)) => {
            if a.get_version() > b.get_version() {
                (a, RotSlot::A)
            } else {
                (b, RotSlot::B)
            }
        }
    }
}

/// Handoff Image metadata to USB SRAM
pub fn dump_image_details_to_ram() {
    // Turn on the memory used by the handoff subsystem to dump
    // `RotUpdateDetails` and DICE information required by hubris.
    //
    // This allows hubris tasks to always get valid memory, even if it is all
    // 0's.
    let peripherals = Peripherals::take().unwrap_lite();
    let handoff = Handoff::turn_on(&peripherals.SYSCON);

    let a = get_image_a().map(image_details);
    let b = get_image_b().map(image_details);
    let (_, active) = select_image_to_boot();

    let details = RotUpdateDetails { active, a, b };

    handoff.store(&details);
}

fn image_details(img: Image) -> RotImageDetails {
    let digest = Sha3_256::digest(img.as_bytes()).try_into().unwrap_lite();
    RotImageDetails {
        digest,
        version: img.get_image_version(),
    }
}
