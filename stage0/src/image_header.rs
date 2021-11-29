// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

extern "C" {
    static IMAGEA: ImageHeader;
}

// TODO grab this from lpc55_support or another crate eventually
#[derive(Debug)]
#[repr(C)]
struct CertHeader {
    signature: [u8; 4],
    header_version: u32,
    header_length: u32,
    flags: u32,
    build_number: u32,
    total_image_len: u32,
    certificate_count: u32,
    certificate_table_len: u32,
    key_size: u32,
    // The u32 here represents the start of the key which comes after this
    // structure.
    key: u32,
}

pub fn get_image_a() -> Option<&'static ImageHeader> {
    // Taking the reference to our supposed imagea
    let imagea = unsafe { &IMAGEA };

    // Step 1: check if the flash for this image is actually programmed
    if !imagea.validate() {
        return None;
    }

    // We've validated that the image range should be safe
    let img_start = imagea.get_img_start();

    let table_start = imagea.get_table_start();

    // Step 2: Check that the table pointed to by this image is actually
    // within our image range that we checked before
    if !imagea.check_bounds(table_start) {
        return None;
    }

    // The table is within bounds so accessing it will not cause a fault
    let table = unsafe { &*imagea.get_cert_table() };

    // our 'signature' is the letters cert. If this isn't valid the rest
    // of the data is probably not valid either.
    if table.signature != [0x63, 0x65, 0x72, 0x74] {
        return None;
    }

    let key_start = &table.key as *const u32 as u32;

    // validate that our key is fully programmed
    if !imagea.check_bounds(key_start + table.key_size) {
        return None;
    }

    let sig_addr = img_start + table.total_image_len;

    if !imagea.check_bounds(sig_addr) {
        return None;
    }

    // The address itself is valid so reading this is safe
    let sig_size = unsafe { core::ptr::read_volatile(sig_addr as *const u32) };

    // Check the signature
    if !imagea.check_bounds(sig_addr + 4) {
        return None;
    }

    if !imagea.check_bounds(sig_addr + 4 + sig_size) {
        return None;
    }

    // Check what is supposed to be the full image length
    if !imagea.check_bounds(img_start + table.total_image_len) {
        return None;
    }

    // We can now say that the following are safe
    // - Accessing the full image range
    // - Accessing the key range
    // - Accessing the signature range
    Some(imagea)
}

// The careful observer will note that yes this is just the
// start of an ARMv8m image with extra data shoved in the
// vector table
#[repr(C)]
pub struct ImageHeader {
    sp: u32,
    pc: u32,
    _vector_table: [u8; 24],
    image_length: u32,
    _image_type: u32,
    header_offset: u32,
}

impl ImageHeader {
    pub fn get_img_start(&self) -> u32 {
        self as *const Self as u32
    }

    fn check_bounds(&self, address: u32) -> bool {
        let start = self.get_img_start();

        return address >= start && address < (start + self.image_length);
    }

    fn get_table_start(&self) -> u32 {
        self.get_img_start() + self.header_offset
    }

    /// Make sure all of the image flash is programmed
    fn validate(&self) -> bool {
        let img_start = self.get_img_start();

        // Start by making sure the region is actually programmed
        let valid = lpc55_romapi::validate_programmed(img_start, 0x200);

        if !valid {
            return false;
        }

        // Next make sure the marked image length is programmed
        let valid = lpc55_romapi::validate_programmed(
            img_start,
            (self.image_length + 0x1ff) & !(0x1ff),
        );

        if !valid {
            return false;
        }

        return true;
    }

    fn get_cert_table(&self) -> *const CertHeader {
        self.get_table_start() as *const CertHeader
    }

    pub fn get_pc(&self) -> u32 {
        self.pc
    }

    pub fn get_sp(&self) -> u32 {
        self.sp
    }
}
