use bitfield::bitfield;
use byteorder::ByteOrder;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::Path;

bitfield! {
    #[derive(Serialize, Deserialize)]
    pub struct BootField(u32);
    impl Debug;

    img_type, set_imgtype : 7, 0;
    reserved1, _ : 12, 8;
    tzm_preset, set_tzm_preset : 13;
    tzm_image_type, set_tzm_image_type : 14;
    blank, _: 31, 15;
}

fn crc32_compute_table() -> [u32; 256] {
    let mut crc32_table = [0; 256];

    for n in 0..256 {
        crc32_table[n as usize] =
            (0..8).rev().fold((n << 24) as u32, |acc, _| {
                match acc & 0x80000000 {
                    0x80000000 => 0x04c11db7 ^ (acc << 1),
                    _ => acc << 1,
                }
            });
    }

    crc32_table
}

fn crc32(buf: Vec<u8>, mut acc: u32) -> u32 {
    let crc_table = crc32_compute_table();

    for b in buf.iter() {
        acc =
            (acc << 8) ^ crc_table[(((acc >> 24) & 0xff) ^ *b as u32) as usize];
    }

    return acc;
}

pub fn update_crc(src: &Path, dest: &Path) -> Result<(), Box<dyn Error>> {
    let mut bytes = std::fs::read(src)?;

    // We need to update 3 fields before calculating the CRC:
    //
    // 0x20 = image length (4 bytes)
    // 0x24 = image type (4 bytes)
    // 0x34 = image execution address (4 bytes)
    //
    // The crc gets placed at 0x28. For other types of images the CRC is a
    // pointer where the key data lives
    //
    let len = bytes.len();

    byteorder::LittleEndian::write_u32(&mut bytes[0x20..0x24], len as u32);
    // indicates TZ image and plain CRC XIP image
    // See 7.5.3.1 for details on why we need the TZ bit
    let mut boot_field = BootField(0);

    // Table 183, section 7.3.4 = CRC Image
    boot_field.set_imgtype(0x5);

    bytes[0x24..0x28]
        .clone_from_slice(&bincode::serialize(&boot_field).unwrap());

    // Our execution address is always 0
    byteorder::LittleEndian::write_u32(&mut bytes[0x34..0x38], 0x0);

    // Now calculate the CRC on everything except the bytes where the CRC goes
    let crc = crc32(
        bytes[0x2c..].to_vec(),
        crc32(bytes[0..0x28].to_vec(), 0xffffffff),
    );

    byteorder::LittleEndian::write_u32(&mut bytes[0x28..0x2c], crc);

    std::fs::write(dest, &bytes)?;
    Ok(())
}
