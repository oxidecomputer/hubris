extern crate packed_struct;
#[macro_use]
extern crate packed_struct_codegen;

use packed_struct::prelude::*;

#[derive(PackedStruct)]
#[packed_struct(size_bytes = "4", bit_numbering = "msb0")]
pub struct BootField {
    #[packed_field(bits = "0..8")]
    pub img_type: Integer<u8, packed_bits::Bits8>,
    #[packed_field(bits = "13")]
    pub tzm_preset: bool,
    #[packed_field(bits = "14")]
    pub tzm_image_type: bool,
}

#[repr(C)]
#[derive(Default, Debug, Clone, PackedStruct)]
#[packed_struct(size_bytes = "4", endian = "msb", bit_numbering = "msb0")]
pub struct CCSOCUPin {
    #[packed_field(bits = "0")]
    niden: bool,
    #[packed_field(bits = "1")]
    dbgen: bool,
    #[packed_field(bits = "2")]
    spniden: bool,
    #[packed_field(bits = "3")]
    spiden: bool,
    #[packed_field(bits = "4")]
    tapen: bool,
    #[packed_field(bits = "5")]
    mcm33_dbg_en: bool,
    #[packed_field(bits = "6")]
    isp_cmd_en: bool,
    #[packed_field(bits = "7")]
    fa_cmd_en: bool,
    #[packed_field(bits = "8")]
    me_cmd_en: bool,
    #[packed_field(bits = "9")]
    mcm33_nid_en: bool,
    #[packed_field(bits = "15")]
    uuid_check: bool,
}

#[derive(Default, Debug, Clone, PackedStruct)]
#[packed_struct(size_bytes = "4", bit_numbering = "msb0")]
pub struct CCSOCUDFLT {
    #[packed_field(bits = "0")]
    niden: bool,
    #[packed_field(bits = "1")]
    dbgen: bool,
    #[packed_field(bits = "2")]
    spniden: bool,
    #[packed_field(bits = "3")]
    spiden: bool,
    #[packed_field(bits = "4")]
    tapen: bool,
    #[packed_field(bits = "5")]
    mcm33_dbg_en: bool,
    #[packed_field(bits = "6")]
    isp_cmd_en: bool,
    #[packed_field(bits = "7")]
    fa_cmd_en: bool,
    #[packed_field(bits = "8")]
    me_cmd_en: bool,
    #[packed_field(bits = "9")]
    mcm33_nid_en: bool,
}

#[derive(Default, Debug, Clone, PackedStruct)]
#[packed_struct(size_bytes = "4", endian = "lsb", bit_numbering = "lsb0")]
pub struct SecureBootCfg {
    #[packed_field(bits = "0..=1")]
    pub rsa4k: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "2..=3")]
    pub dice_inc_nxp_cfg: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "4..=5")]
    pub dice_cust_cfg: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "6..=7")]
    pub skip_dice: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "8..=9")]
    pub tzm_image_type: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "10..=11")]
    pub block_set_key: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "12..=13")]
    pub block_enroll: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "14..=15")]
    pub dice_inc_sec_epoch: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "30..=31")]
    pub sec_boot_en: Integer<u8, packed_bits::Bits2>,
}

#[repr(C)]
#[derive(Default, Debug, Clone, PackedStruct)]
#[packed_struct(size_bytes = "512", bit_numbering = "msb0", endian = "msb")]
pub struct CMPAPage {
    // Features settings such as a boot failure pin, boot speed and
    // default ISP mode. Okay to leave at 0x0
    boot_cfg: u32,
    // Undocumented what this does
    spi_flash_cfg: u32,
    // Can set vendor/product ID
    usb_id: u32,
    // Undocumented what this does
    sdio_cfg: u32,
    // Can turn off various peripherals
    // This needs to be kept 0!
    #[packed_field(size_bytes = "4", bytes = "0x10:0x13")]
    cc_socu_pin: CCSOCUPin,
    #[packed_field(size_bytes = "4", bytes = "0x14:0x17")]
    cc_socu_dflt: CCSOCUDFLT,
    // Related to secure debug
    vendor_usage: u32,
    // Sets boot mode
    #[packed_field(size_bytes = "4", bytes = "0x1c:0x1f")]
    pub secure_boot_cfg: SecureBootCfg,
    // prince settings
    prince_base_addr: u32,
    prince_sr_0: u32,
    prince_sr_1: u32,
    prince_sr_2: u32,
    // These are listed in the manual but not documented at all
    xtal_32khz_capabank_trim: u32,
    xtal_16khz_capabank_trim: u32,
    flash_remap_size: u32,
    blank1: [u8; 0x14],
    // The hash of the RoT keys
    rotkh7: u32,
    rotkh6: u32,
    rotkh5: u32,
    rotkh4: u32,
    rotkh3: u32,
    rotkh2: u32,
    rotkh1: u32,
    rotkh0: u32,
    // Let's see if it likes this
    blank2: [u8; 32],
    blank3: [u8; 32],
    blank4: [u8; 32],
    blank5: [u8; 32],
    blank6: [u8; 16],
    customer_defined0: [u8; 32],
    customer_defined1: [u8; 32],
    customer_defined2: [u8; 32],
    customer_defined3: [u8; 32],
    customer_defined4: [u8; 32],
    customer_defined5: [u8; 32],
    customer_defined6: [u8; 32],
    // !!! DO NOT WRITE THIS !!!
    // This will prevent re-writing!
    sha256_digest: [u8; 32],
}

#[derive(Clone, Debug, PackedStruct)]
#[repr(C)]
#[packed_struct(size_bytes = "0x20", bit_numbering = "msb0", endian = "msb")]
pub struct CertHeader {
    pub signature: [u8; 4],
    #[packed_field(endian = "lsb")]
    pub header_version: u32,
    #[packed_field(endian = "lsb")]
    pub header_length: u32,
    #[packed_field(endian = "lsb")]
    pub flags: u32,
    #[packed_field(endian = "lsb")]
    pub build_number: u32,
    #[packed_field(endian = "lsb")]
    pub total_image_len: u32,
    #[packed_field(endian = "lsb")]
    pub certificate_count: u32,
    #[packed_field(endian = "lsb")]
    pub certificate_table_len: u32,
}

#[derive(Clone, Debug, PackedStruct, Default)]
#[repr(C)]
#[packed_struct(size_bytes = "4", bit_numbering = "lsb0")]
pub struct RKTHRevoke {
    #[packed_field(bits = "0..=1")]
    pub rotk0: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "2..=3")]
    pub rotk1: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "4..=5")]
    pub rotk2: Integer<u8, packed_bits::Bits2>,
    #[packed_field(bits = "6..=7")]
    pub rotk3: Integer<u8, packed_bits::Bits2>,
}

#[derive(Clone, Debug, PackedStruct, Default)]
#[repr(C)]
#[packed_struct(size_bytes = "512", bit_numbering = "msb0", endian = "msb")]
pub struct CFPAPage {
    // Unclear what this header does. Leaving as 0 is fine
    header: u32,
    // Monotonically incrementing version counter. This
    // _must_ be incremented on every update!
    #[packed_field(endian = "lsb")]
    pub version: u32,
    // Both fields are related to signed update (sb2)
    // loading. This must be equal or lower than the
    // version specified in the update file
    secure_firwmare_version: u32,
    ns_fw_version: u32,
    // Used to revoke certificates, see 7.3.2.1.2 for
    // details. Keep as 0 for now.
    image_key_revoke: u32,
    reserved: u32,
    // Used for revoking individual keys
    #[packed_field(endian = "lsb", size_bytes = "4", bytes = "0x18:0x1b")]
    pub rotkh_revoke: RKTHRevoke,
    // Used for debug authentication
    vendor: u32,
    // Turn peripherals off and on. Leaving as default
    // leaves everything enabled.
    #[packed_field(size_bytes = "4", bytes = "0x20:0x23")]
    dcfg_cc_socu_ns_pin: CCSOCUPin,
    #[packed_field(size_bytes = "4", bytes = "0x24:0x27")]
    dcfg_cc_socu_ns_dflt: CCSOCUDFLT,
    // Set fault analysis mode
    enable_fa_mode: u32,
    // From the sheet
    // "CMPA Page programming on going. This field shall be set to 0x5CC55AA5
    // in the active CFPA page each time CMPA page programming is going on. It
    // shall always be set to 0x00000000 in the CFPA scratch area.
    cmpa_prog_in_progress: u32,
    // prince security codes. These are split up to get around rust's
    // limitation of 256 byte arrays
    prince_region0_code0: [u8; 0x20],
    prince_region0_code1: [u8; 0x18],
    prince_region1_code0: [u8; 0x20],
    prince_region1_code1: [u8; 0x18],
    prince_region2_code0: [u8; 0x20],
    prince_region2_code1: [u8; 0x18],
    // More blank space!
    mysterious1: [u8; 0x20],
    mysterious2: [u8; 0x8],
    // Rust absolutely hates 256 bytes arrays
    customer_defined0: [u8; 32],
    customer_defined1: [u8; 32],
    customer_defined2: [u8; 32],
    customer_defined3: [u8; 32],
    customer_defined4: [u8; 32],
    customer_defined5: [u8; 32],
    customer_defined6: [u8; 32],
    // This needs to be updated every time
    sha256_digest: [u8; 32],
}
