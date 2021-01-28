use crate::puf::*;
use crate::ImageHeader;
use hmac::{Hmac, Mac, NewMac};
use lpc55_pac as device;
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use zerocopy::AsBytes;

fn get_key_from_puf(key: &mut [u32]) -> Result<(), ()> {
    let puf = unsafe { &*device::PUF::ptr() };
    let syscon = unsafe { &*device::SYSCON::ptr() };

    let mut activation_code = [0u32; 298];

    let mut key_code = [0u32; 13];

    puf_init(puf, syscon)?;

    puf_enroll(puf, &mut activation_code)?;

    turn_off_puf(puf, syscon);

    puf_init(puf, syscon)?;

    puf_start(puf, &activation_code)?;

    puf_set_intrinsic_key(puf, 1, 256, &mut key_code)?;

    puf_get_key(puf, 1, &key_code, key)?;

    Ok(())
}

#[repr(C)]
pub struct AttestInfo {
    // hash of the image
    img_hash: [u8; 32],
    // our boot nonce
    nb: u32,
    // entry point
    entry_pt: u32,
    // image size
    image_size: u32,
    // padding
    _reserved: u32,
    // our next level attestation key
    ak1: [u8; 32],
}

#[link_section = ".attestation"]
static mut ATTESTATION: AttestInfo = AttestInfo {
    img_hash: [0; 32],
    nb: 0,
    entry_pt: 0,
    image_size: 0,
    _reserved: 0,
    ak1: [0; 32],
};

/// Calculate an attestation for someone to check our work later
pub fn attest(
    image_size: u32,
    image_hash: &[u8],
    entry_pt: u32,
) -> Result<(), ()> {
    let mut key = [0u32; 8];

    get_key_from_puf(&mut key)?;

    let rom_hash = unsafe {
        sha2::Sha256::digest(core::slice::from_raw_parts(
            0x13000000 as *const u8,
            0x20000 as usize,
        ))
    };

    let mut mac = Hmac::<Sha256>::new_varkey(key.as_bytes()).unwrap();

    // https://xkcd.com/221/
    // Yes we will fix this later;
    let boot_nonce: u32 = 4;

    mac.update(rom_hash.as_slice());
    mac.update(&image_size.to_le_bytes());
    mac.update(image_hash);
    mac.update(&entry_pt.to_le_bytes());
    mac.update(&boot_nonce.to_le_bytes());

    let m1 = mac.finalize().into_bytes();

    // We're writing to our global attestation variable. We only write it
    // here and expect it to be read later from hubris
    unsafe {
        ATTESTATION.nb = boot_nonce;
        ATTESTATION.entry_pt = entry_pt;
        ATTESTATION.image_size = image_size;
        ATTESTATION.img_hash.clone_from_slice(image_hash);
        ATTESTATION.ak1.clone_from_slice(&m1);
    }

    Ok(())
}

// TODO grab this from lpc55_support or another crate eventually
#[derive(Debug)]
#[repr(C)]
pub struct CertHeader {
    pub signature: [u8; 4],
    pub header_version: u32,
    pub header_length: u32,
    pub flags: u32,
    pub build_number: u32,
    pub total_image_len: u32,
    pub certificate_count: u32,
    pub certificate_table_len: u32,
    pub key_size: u32,
    // The u32 here represents the start of the key which comes after this
    // structure.
    pub key: u32,
}

impl ImageHeader {
    fn get_cert_table(&self) -> *const CertHeader {
        let img_start = self.get_img_start();

        let table_start = img_start + self.header_offset;

        table_start as *const CertHeader
    }

    fn get_key_bytes(&self) -> &[u8] {
        let table = unsafe { &*self.get_cert_table() };

        unsafe {
            // This is ugly since it is technially outside the bounds of the
            // structure but it only ever gets read and we have verified that
            // the region was programmed so it should not hard fault
            core::slice::from_raw_parts(
                &table.key as *const u32 as *const u8,
                table.key_size as usize,
            )
        }
    }

    fn get_signature_bytes(&self) -> &[u8] {
        let img_start = self.get_img_start();
        let table = unsafe { &*self.get_cert_table() };

        let sig_addr = img_start + table.total_image_len;
        let sig_size =
            unsafe { core::ptr::read_volatile(sig_addr as *const u32) };
        unsafe {
            // The signature exists right after the image bytes
            core::slice::from_raw_parts(
                (sig_addr + 4) as *const u8,
                sig_size as usize,
            )
        }
    }

    fn get_image_bytes(&self) -> &[u8] {
        let img_start = self.get_img_start();
        let table = unsafe { &*self.get_cert_table() };

        unsafe {
            core::slice::from_raw_parts(
                img_start as *const u8,
                table.total_image_len as usize,
            )
        }
    }

    fn get_total_len(&self) -> u32 {
        let table = unsafe { &*self.get_cert_table() };
        table.total_image_len
    }
}

/// Validate the signature of the image at the specified address.
/// This involves checking against the structure as specified by NXP as well as
/// validating a signature. Currently using an ecdsa signature.
pub fn validate_image(
    image: &ImageHeader,
    image_size: &mut u32,
    image_hash: &mut [u8],
    entry_pt: &mut u32,
    stack: &mut u32,
) -> Result<(), ()> {
    let key_bytes = image.get_key_bytes();
    let verifying_key = VerifyingKey::from_sec1_bytes(&key_bytes).unwrap();

    let sig_bytes = image.get_signature_bytes();
    let sig = Signature::from_asn1(&sig_bytes);

    let image_bytes = image.get_image_bytes();

    let valid = verifying_key.verify(&image_bytes, &sig.unwrap());

    if valid.is_err() {
        return Err(());
    }

    *image_size = image.get_total_len();
    *entry_pt = image.pc;
    *stack = image.sp;

    let hash = sha2::Sha256::digest(&image_bytes);

    image_hash.copy_from_slice(hash.as_slice());

    Ok(())
}
