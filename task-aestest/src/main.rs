#![no_std]
#![no_main]
#![feature(llvm_asm)]

use cortex_m_semihosting::hprintln;
use sha1;
use userlib::*;

#[cfg(feature = "standalone")]
const AES: Task = SELF;

#[cfg(not(feature = "standalone"))]
const AES: Task = Task::aes_driver;

// Values from the NIST test vectors

const CIPHER_TEXT: [u8; 4 * 16] = [
    0x3a, 0xd7, 0x7b, 0xb4, 0x0d, 0x7a, 0x36, 0x60, 0xa8, 0x9e, 0xca, 0xf3,
    0x24, 0x66, 0xef, 0x97, 0xf5, 0xd3, 0xd5, 0x85, 0x03, 0xb9, 0x69, 0x9d,
    0xe7, 0x85, 0x89, 0x5a, 0x96, 0xfd, 0xba, 0xaf, 0x43, 0xb1, 0xcd, 0x7f,
    0x59, 0x8e, 0xce, 0x23, 0x88, 0x1b, 0x00, 0xe3, 0xed, 0x03, 0x06, 0x88,
    0x7b, 0x0c, 0x78, 0x5e, 0x27, 0xe8, 0xad, 0x3f, 0x82, 0x23, 0x20, 0x71,
    0x04, 0x72, 0x5d, 0xd4,
];

const KEY: [u8; 16] = [
    0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88,
    0x09, 0xcf, 0x4f, 0x3c,
];

const PLAIN_TEXT: [u8; 4 * 16] = [
    0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11,
    0x73, 0x93, 0x17, 0x2a, 0xae, 0x2d, 0x8a, 0x57, 0x1e, 0x03, 0xac, 0x9c,
    0x9e, 0xb7, 0x6f, 0xac, 0x45, 0xaf, 0x8e, 0x51, 0x30, 0xc8, 0x1c, 0x46,
    0xa3, 0x5c, 0xe4, 0x11, 0xe5, 0xfb, 0xc1, 0x19, 0x1a, 0x0a, 0x52, 0xef,
    0xf6, 0x9f, 0x24, 0x45, 0xdf, 0x4f, 0x9b, 0x17, 0xad, 0x2b, 0x41, 0x7b,
    0xe6, 0x6c, 0x37, 0x10,
];

#[export_name = "main"]
fn main() -> ! {
    let aes = TaskId::for_index_and_gen(AES as usize, Generation::default());

    hprintln!("Starting HashCrypt test").ok();

    let mut out: [u8; 4 * 16] = [0; 4 * 16];
    let a: &mut [u8] = &mut out;
    let p: &[u8] = &PLAIN_TEXT;
    let (code, _) =
        sys_send(aes, 1, &KEY, &mut [], &[Lease::from(p), Lease::from(a)]);
    if code != 0 {
        hprintln!("Got error code{}", code).ok();
    } else {
        let result = out
            .iter()
            .zip(CIPHER_TEXT.iter())
            .map(|(a, b)| a == b)
            .fold(true, |stat, next| stat && next);
        if !result {
            hprintln!("cipher fail").ok();
        }
    }

    // Generate some arrays of all 'a' to do some testing of sha1
    for i in 1..128 {
        let mut hasher = sha1::Sha1::new();
        let mut arr: [u8; 128] = [0; 128];

        for j in 0..i {
            arr[j] = 0x61;
        }

        hasher.update(&arr[..i]);

        let expect = hasher.digest().bytes();

        let mut digest: [u8; 4 * 5] = [0; 4 * 5];
        let a: &mut [u8] = &mut digest;

        let p: &[u8] = &arr[..i];

        let (code, _) =
            sys_send(aes, 2, &[], &mut [], &[Lease::from(p), Lease::from(a)]);

        if code != 0 {
            hprintln!("Got error code{}", code).ok();
        } else {
            let result = digest
                .iter()
                .zip(expect.iter())
                .map(|(a, b)| a == b)
                .fold(true, |stat, next| stat && next);
            if !result {
                hprintln!("len {} failed", i).ok();
                hprintln!("expected {:x?}", expect).ok();
                hprintln!("got {:x?}", digest).ok();
            }
        }
    }

    // Another test vector from NIST
    let p: &[u8] =
        "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".as_bytes();
    let mut digest: [u8; 4 * 5] = [0; 4 * 5];
    let a: &mut [u8] = &mut digest;

    let mut hasher = sha1::Sha1::new();

    hasher.update(&p);

    let expect = hasher.digest().bytes();

    let (code, _) =
        sys_send(aes, 2, &[], &mut [], &[Lease::from(p), Lease::from(a)]);

    if code != 0 {
        hprintln!("Got error code{}", code).ok();
    } else {
        let result = digest
            .iter()
            .zip(expect.iter())
            .map(|(a, b)| a == b)
            .fold(true, |stat, next| stat && next);
        if !result {
            hprintln!("expected {:x?}", expect).ok();
            hprintln!("got {:x?}", digest).ok();
        }
    }

    hprintln!("done.").ok();
    loop {}
}
