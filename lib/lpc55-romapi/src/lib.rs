// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![feature(naked_functions)]
#![no_std]

use num_derive::FromPrimitive;
use num_traits::FromPrimitive;

#[repr(u32)]
#[derive(Debug, FromPrimitive, PartialEq, Eq, Clone, Copy)]
pub enum FlashStatus {
    Success = 0,
    InvalidArg = 4,
    SizeError = 100,
    AlignmentError = 101,
    AddressError = 102,
    AccessError = 103,
    ProtectionViolation = 104,
    CommandFailure = 105,
    UnknownProperty = 106,
    EraseKeyError = 107,
    ExecuteOnly = 108,
    InRamNotReady = 109,
    CommandNotSupported = 111,
    ReadOnlyProperty = 112,
    InvalidProperty = 113,
    InvalidSpeculation = 114,
    ECCError = 116,
    CompareError = 117,
    RegulationLoss = 118,
    InvalidWaitState = 119,
    OutOfDateCFPA = 132,
    BlankIFRPage = 133,
    EncryptedRegionsEraseNotDone = 134,
    ProgramVerificationNotAllowed = 135,
    HashCheckError = 136,
    SealedFFR = 137,
    FFRWriteBroken = 138,
    NMPAAccessNotAllowed = 139,
    CMPADirectEraseNotAllowed = 140,
    FFRBankIsLocked = 141,
    /// Used to encode an unknown return from the flash status, not defined
    /// by NXP
    Unknown = 255,
}

#[repr(u32)]
pub enum FFRKeyType {
    /// Secure Boot Key Code, used for validation when flashing with
    /// the signed capsule update
    SBKek = 0x0,
    /// Generic User Key Code to be used for anything
    User = 0x1,
    /// Universal Device Secret, used for the unsupported DICE feature
    UDS = 0x2,
    /// For encryption
    PrinceRegion0 = 0x3,
    PrinceRegion1 = 0x4,
    Princeregion2 = 0x5,
}

#[repr(u32)]
#[derive(Debug, FromPrimitive, PartialEq, Eq)]
pub enum BootloaderStatus {
    Success = 0,
    // This one is technically not in the ROM API space but testing has
    // shown this gets returned when the scratch buffer is not big enough
    Fail = 1,
    NeedMoreData = 10801,
    BufferNotBigEnough = 10802,
    InvalidBuffer = 10803,
    Unknown = 255,
}

#[repr(C)]
#[derive(Default, Debug)]
struct StandardVersion {
    bugfix: u8,
    minor: u8,
    major: u8,
    name: u8,
}

#[repr(C)]
pub struct FfrKeyStore {
    header: u32,
    puf_discharge: u32,
    activation_code: [u32; ACTIVATION_CODE_SIZE / 4],
    sb_header: u32,
    sb_key_code: [u32; 13],
    user_header: u32,
    user_key_code: [u32; 13],
    uds_header: u32,
    uds_key_code: [u32; 13],
    prince0_header: u32,
    prince0_key_code: [u32; 13],
    prince1_header: u32,
    prince1_key_code: [u32; 13],
    prince2_header: u32,
    prince2_key_code: [u32; 13],
}

pub const FLASH_PAGE_SIZE: usize = 512;

const ACTIVATION_CODE_SIZE: usize = 1192;

// - Start addresses and lengths are given as u32 as this results in the
//   fewest casts given the amount of math needed to read/write from the
//   correct addresses.
// - The official documentation uses uint8_t * in some places and uint32_t *
//   in others. Using the larger alignment is safer so *mut u32 is used for
//   buffers.  Many of these are marked as mut out of extreme caution even
//   though they shouldn't be modified at all.
#[repr(C)]
struct Version1DriverInterface {
    version: StandardVersion,
    /// flash_init: Set up function that must be called before any other
    /// flash function. Note: set the CPU frequency in the flash config
    /// otherwise the flash refresh rate may not be correct!
    flash_init: unsafe extern "C" fn(config: &mut FlashConfig) -> u32,
    /// flash_erase: Erases the flash. Need to pass ERASE_KEY for this to work
    flash_erase: unsafe extern "C" fn(
        config: &mut FlashConfig,
        start: u32,
        length: u32,
        key: u32,
    ) -> u32,
    /// flash_program: write the bytes to flash
    flash_program: unsafe extern "C" fn(
        config: &mut FlashConfig,
        start: u32,
        src: *mut u8,
        length: u32,
    ) -> u32,
    /// flash_verify_erase: Verify that the region is actually erased
    flash_verify_erase: unsafe extern "C" fn(
        config: &mut FlashConfig,
        start: u32,
        length: u32,
    ) -> u32,
    /// flash_verify_program: Verify that the region was programed
    flash_verify_program: unsafe extern "C" fn(
        config: &mut FlashConfig,
        start: u32,
        length: u32,
        expectedData: *mut u32,
        failedAddress: &mut u32,
        failedData: &mut u32,
    ) -> u32,
    /// flash_get_property: Get a particular value from the flash. whichProperty
    /// is technically an enum.
    flash_get_property: unsafe extern "C" fn(
        config: &mut FlashConfig,
        whichProperty: u32,
        value: &mut u32,
    ) -> u32,
    // Why yes these two structures differ by several reserved words!
    reserved: [u32; 3],
    /// ffr_init: Initialize the FFR structure (needs to run before other
    /// functions)
    ffr_init: unsafe extern "C" fn(config: &mut FlashConfig) -> u32,
    /// ffr_deinit: Prevent further writes to the protected flash for this
    /// boot. Will reset on power cycle
    ffr_deinit: unsafe extern "C" fn(config: &mut FlashConfig) -> u32,
    /// write to the CMPA -- Don't write seal part unless you want to
    /// prevent further writes
    ffr_cust_factory_page_write: unsafe extern "C" fn(
        config: &mut FlashConfig,
        page_data: &[u8; FLASH_PAGE_SIZE],
        seal_part: bool,
    ) -> u32,
    /// Get the UUID from the NXP area
    ffr_get_uuid: unsafe extern "C" fn(
        config: &mut FlashConfig,
        uuid: &mut [u8; 32],
    ) -> u32,
    /// Read data from the CMPA (aka factory page)
    ffr_get_customer_data: unsafe extern "C" fn(
        config: &mut FlashConfig,
        pdata: *mut u32,
        offset: u32,
        len: u32,
    ) -> u32,
    /// Write to the keystore
    ffr_keystore_write: unsafe extern "C" fn(
        config: &mut FlashConfig,
        pKeyStore: *mut FfrKeyStore,
    ) -> u32,
    /// get the activation code
    ffr_keystore_get_ac: unsafe extern "C" fn(
        config: &mut FlashConfig,
        ac: *mut [u32; ACTIVATION_CODE_SIZE / 4],
    ) -> u32,
    /// get a particular key code
    ffr_keystore_get_kc: unsafe extern "C" fn(
        config: &mut FlashConfig,
        pKeyCode: &[u32; 13],
        key_index: u32,
    ) -> u32,
    /// write to the CFPA aka in-field page
    ffr_infield_page_write: unsafe extern "C" fn(
        config: &mut FlashConfig,
        page_data: &[u32; FLASH_PAGE_SIZE / 4],
        valid_len: u32,
    ) -> u32,
    /// read the CFPA aka in-field page
    ffr_get_customer_infield_data: unsafe extern "C" fn(
        config: &mut FlashConfig,
        pdata: *mut u32,
        offset: u32,
        len: u32,
    ) -> u32,
}

#[derive(Debug)]
#[repr(C)]
struct KBSessionRef {
    context: KBOptions,
    cau3initialized: bool,
    memory_map: u32, // XXX What's this structure definition?
}

#[repr(C)]
#[derive(Debug)]
struct KBOptions {
    version: u32,
    buffer: *const u8,
    buffer_len: u32,
    op: u32, // XXX NXP does not define this enum
    load_sb: KBLoadSb,
}

#[repr(C)]
#[derive(Debug)]
struct KBLoadSb {
    profile: u32,
    min_build: u32,
    override_sb_section: u32,
    user_sb: u32, // XXX I think this could be NULL
    region_cnt: u32,
    regions: u32, // XXX What's this structure definition?
}

// Both SkbootStatus and SecureBool are defined in the NXP manual

#[repr(u32)]
#[derive(Debug, FromPrimitive, PartialEq)]
enum SkbootStatus {
    Success = 0x5ac3c35a,
    Fail = 0xc35ac35a,
    InvalidArgument = 0xc35a5ac3,
    KeyStoreMarkerInvalid = 0xc3c35a5a,
}

#[repr(u32)]
#[derive(Debug, FromPrimitive, PartialEq)]
enum SecureBool {
    SecureFalse = 0x5aa55aa5,
    SecureTrue = 0xc33cc33c,
    TrackerVerified = 0x55aacc33,
}

#[repr(C)]
struct IAPInterface {
    kb_init: unsafe extern "C" fn(
        session: *mut *mut KBSessionRef,
        options: *const KBOptions,
    ) -> u32,

    kb_deinit: unsafe extern "C" fn(session: *mut KBSessionRef) -> u32,

    kb_execute: unsafe extern "C" fn(
        session: *mut KBSessionRef,
        data: *mut u8,
        len: u32,
    ) -> u32,
}

#[repr(C)]
struct FlashDriverInterface {
    /// This is technically a union for the v0 vs v1 ROM but we only care
    /// about the v1 on the Expresso board
    version1_flash_driver: &'static Version1DriverInterface,
}

#[repr(C)]
struct SKBootFns {
    skboot_authenticate: unsafe extern "C" fn(
        start_addr: *const u32,
        is_verified: *mut u32,
    ) -> u32,
    skboot_hashcrypt_irq_handler: unsafe extern "C" fn() -> (),
}

#[repr(C)]
struct BootloaderTree {
    /// Function to start the bootloader executing
    bootloader_fn: unsafe extern "C" fn(*const u8),
    /// Bootloader version
    version: StandardVersion,
    /// Actually a C string but we don't have that in no-std
    copyright: *const u8,
    reserved: u32,
    /// Functions for reading/writing to flash
    flash_driver: FlashDriverInterface,
    /// Functions for working with signed capsule updates
    iap_driver: &'static IAPInterface,
    reserved1: u32,
    reserved2: u32,
    /// Functions for low power settings, used in conjunction with a
    /// binary shared lib, (might add function prototypes later)
    low_power: u32,
    /// Functions for PRINCE encryption, currently not implemented
    crypto: u32,
    /// Functions for checking signatures on images
    skboot: &'static SKBootFns,
}

// We need to call this function when using either skboot_authenticate or
// the sb2 exec function
pub unsafe extern "C" fn skboot_hashcrypt_handler() {
    (bootloader_tree().skboot.skboot_hashcrypt_irq_handler)();
}

#[repr(C)]
#[derive(Default, Debug)]
struct ReadSingleWord {
    /// This is technically a bitfield but if we need bits later we
    /// can set it up
    field: u32,
}

#[repr(C)]
#[derive(Default, Debug)]
struct SetWriteMode {
    program_ramp_control: u8,
    erase_ramp_control: u8,
    reserved: [u8; 2],
}

#[repr(C)]
#[derive(Default, Debug)]
struct SetReadMode {
    read_interface_timing_trim: u16,
    read_controller_timing_trim: u16,
    read_wait_wtates: u8,
    reserved: [u8; 3],
}

#[repr(C)]
#[derive(Default, Debug)]
struct FlashModeConfig {
    /// This is an input! The refresh rate gets set based off of this
    sys_freq_in_mhz: u32,
    /// All of these are settings we can set but nothing in the example
    /// driver sets them so we can probably just leave it as is
    _read_single_word: ReadSingleWord,
    _set_write_mode: SetWriteMode,
    _set_read_mode: SetReadMode,
}

#[repr(C)]
#[derive(Default, Debug)]
struct FlashFFRConfig {
    ffr_block_base: u32,
    ffr_total_size: u32,
    ffr_page_size: u32,
    cfpa_page_version: u32,
    cfpa_page_offset: u32,
}

#[repr(C)]
#[derive(Default, Debug)]
struct FlashConfig {
    pflash_block_base: u32,
    pflash_total_size: u32,
    pflash_block_count: u32,
    pflash_page_size: u32,
    pflash_sector_size: u32,
    ffr_config: FlashFFRConfig,
    mode_config: FlashModeConfig,
}

fn get_system_clock_speed_mhz() -> u32 {
    let syscon = unsafe { &*lpc55_pac::SYSCON::ptr() };

    let a = syscon.mainclksela.read().bits();
    let b = syscon.mainclkselb.read().bits();
    let div = syscon.ahbclkdiv.read().bits();

    // corresponds to FRO 96 MHz, see 4.5.34 in user manual
    const EXPECTED_MAINCLKSELA: u32 = 3;
    // corresponds to Main Clock A, see 4.5.45 in user manual
    const EXPECTED_MAINCLKSELB: u32 = 0;

    // We expect the 96MHz clock to be used based on the ROM.
    // If it's not there are probably more (bad) surprises coming
    // and panicking is reasonable
    if a != EXPECTED_MAINCLKSELA || b != EXPECTED_MAINCLKSELB {
        panic!();
    }

    if div == 0 {
        96
    } else {
        48
    }
}

// Magic from the docs!
const ERASE_KEY: u32 = 0x6b65666c;

const LPC55_ROM_TABLE: *const BootloaderTree =
    0x130010f0 as *const BootloaderTree;

fn bootloader_tree() -> &'static BootloaderTree {
    unsafe { &*(LPC55_ROM_TABLE) }
}

const LPC55_BOOT_ROM: *const BootRom = 0x0300_0000 as *const BootRom;

#[repr(C)]
pub struct BootRom {
    pub data: [u8; 0x00010000],
}

pub fn bootrom() -> &'static BootRom {
    unsafe { &*(LPC55_BOOT_ROM) }
}

fn handle_flash_status(ret: u32) -> Result<(), FlashStatus> {
    let result = match FlashStatus::from_u32(ret) {
        Some(a) => a,
        None => return Err(FlashStatus::Unknown),
    };

    match result {
        FlashStatus::Success => Ok(()),
        a => Err(a),
    }
}

fn handle_skboot_status(ret: u32) -> Result<(), ()> {
    let result = match SkbootStatus::from_u32(ret) {
        Some(a) => a,
        None => return Err(()),
    };

    match result {
        SkbootStatus::Success => Ok(()),
        _ => Err(()),
    }
}

fn handle_secure_bool(ret: u32) -> Result<(), ()> {
    let result = match SecureBool::from_u32(ret) {
        Some(a) => a,
        None => return Err(()),
    };

    // This looks odd in that true is also a failure
    match result {
        SecureBool::TrackerVerified => Ok(()),
        _ => Err(()),
    }
}

fn handle_bootloader_status(ret: u32) -> Result<(), BootloaderStatus> {
    let result = match BootloaderStatus::from_u32(ret) {
        Some(a) => a,
        None => return Err(BootloaderStatus::Unknown),
    };

    match result {
        BootloaderStatus::Success => Ok(()),
        a => Err(a),
    }
}

// Checks that address and length are flash page aligned
fn check_addr_len_alignment(addr: u32, len: u32) -> Result<(), FlashStatus> {
    if addr % 512 == 0 && len % 512 == 0 {
        Ok(())
    } else {
        Err(FlashStatus::AlignmentError)
    }
}

#[allow(clippy::result_unit_err)]
pub unsafe fn authenticate_image(addr: u32) -> Result<(), ()> {
    let mut result: u32 = 0;

    let ret = (bootloader_tree().skboot.skboot_authenticate)(
        addr as *const u32,
        &mut result,
    );

    handle_skboot_status(ret)?;

    handle_secure_bool(result)
}

pub unsafe fn load_sb2_image(
    image: &mut [u8],
    scratch_buffer: &mut [u8],
) -> Result<(), BootloaderStatus> {
    // The minimum scratch buffer size seems to be 4096 based on disassembly?
    if scratch_buffer.len() < 0x1000 {
        return Err(BootloaderStatus::Fail);
    }

    let mut context: *mut KBSessionRef = core::ptr::null_mut();

    let mut options = KBOptions {
        version: 1, // Supposed to be kBootApiVersion which isn't defined
        buffer: scratch_buffer.as_mut_ptr(),
        buffer_len: scratch_buffer.len() as u32,
        op: 2, // Corresponds to kRomLoadImage based on disassembly
        load_sb: KBLoadSb {
            profile: 0,
            min_build: 1,
            override_sb_section: 1,
            user_sb: 0, // Currently doesn't support using another key
            region_cnt: 0,
            regions: 0,
        },
    };

    handle_bootloader_status((bootloader_tree().iap_driver.kb_init)(
        &mut context,
        &mut options,
    ))?;

    handle_bootloader_status((bootloader_tree().iap_driver.kb_execute)(
        context,
        image.as_mut_ptr(),
        image.len() as u32,
    ))
}

pub unsafe fn flash_erase(addr: u32, len: u32) -> Result<(), FlashStatus> {
    //   XXX More validation of buffer?
    //   We expect the caller to have dropped the clocks appropriately
    let mut f: FlashConfig = Default::default();
    f.mode_config.sys_freq_in_mhz = get_system_clock_speed_mhz();

    check_addr_len_alignment(addr, len)?;

    handle_flash_status((bootloader_tree()
        .flash_driver
        .version1_flash_driver
        .flash_init)(&mut f))?;

    handle_flash_status((bootloader_tree()
        .flash_driver
        .version1_flash_driver
        .ffr_init)(&mut f))?;

    handle_flash_status((bootloader_tree()
        .flash_driver
        .version1_flash_driver
        .flash_erase)(&mut f, addr, len, ERASE_KEY))
}

pub unsafe fn flash_write(
    addr: u32,
    buffer: *mut u8,
    len: u32,
) -> Result<(), FlashStatus> {
    //   XXX More validation of buffer?
    //   XXX docs say we need to drop the clocks?
    let mut f: FlashConfig = Default::default();
    f.mode_config.sys_freq_in_mhz = get_system_clock_speed_mhz();

    check_addr_len_alignment(addr, len)?;

    handle_flash_status((bootloader_tree()
        .flash_driver
        .version1_flash_driver
        .flash_init)(&mut f))?;

    handle_flash_status((bootloader_tree()
        .flash_driver
        .version1_flash_driver
        .ffr_init)(&mut f))?;

    // XXX so much more validation needed

    handle_flash_status((bootloader_tree()
        .flash_driver
        .version1_flash_driver
        .flash_program)(&mut f, addr, buffer, len))
}

/*
 * The LPC55 will hard fault if it accesses an unprogrammed area. This function
 * uses the ROM APIs to make sure the flash is programmed before we access
 */
pub fn validate_programmed(start: u32, len: u32) -> bool {
    let mut f: FlashConfig = Default::default();
    f.mode_config.sys_freq_in_mhz = get_system_clock_speed_mhz();

    let ret = handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .flash_init)(&mut f)
    });

    if ret.is_err() {
        return false;
    }

    let ret = handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_init)(&mut f)
    });

    if ret.is_err() {
        return false;
    }

    // flash_verify_erased returns true iff all flash pages in the range are
    // erased.  If at least one is programmed, it returns false.  Since we want
    // to know that all pages are programmed, we need to check each page
    // individually.
    let page_size = FLASH_PAGE_SIZE as u32;
    let page_aligned_start = start - (start % page_size);
    let page_aligned_end = {
        let end = start + len;
        (end + page_size - 1) - ((end + page_size - 1) % page_size)
    };

    for page in
        (page_aligned_start..page_aligned_end).step_by(page_size as usize)
    {
        let v = handle_flash_status(unsafe {
            (bootloader_tree()
                .flash_driver
                .version1_flash_driver
                .flash_verify_erase)(&mut f, page, page_size)
        });

        match v {
            // Page is erased
            Ok(_) => return false,
            // Page is programmed
            Err(FlashStatus::CommandFailure) => continue,
            // Some other error was encountered
            Err(_) => return false,
        }
    }

    true
}

pub fn get_key_code(
    idx: FFRKeyType,
    key_code: &mut [u32; 13],
) -> Result<(), FlashStatus> {
    let mut f: FlashConfig = Default::default();
    f.mode_config.sys_freq_in_mhz = get_system_clock_speed_mhz();

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .flash_init)(&mut f)
    })?;

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_init)(&mut f)
    })?;

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_keystore_get_kc)(&mut f, key_code, idx as u32)
    })
}

pub fn get_activation_code(
    ac: &mut [u32; ACTIVATION_CODE_SIZE / 4],
) -> Result<(), FlashStatus> {
    let mut f: FlashConfig = Default::default();
    f.mode_config.sys_freq_in_mhz = get_system_clock_speed_mhz();

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .flash_init)(&mut f)
    })?;

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_init)(&mut f)
    })?;

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_keystore_get_ac)(&mut f, ac)
    })
}

pub fn write_keystore(key_store: &mut FfrKeyStore) -> Result<(), FlashStatus> {
    let mut f: FlashConfig = Default::default();
    f.mode_config.sys_freq_in_mhz = get_system_clock_speed_mhz();

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .flash_init)(&mut f)
    })?;

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_init)(&mut f)
    })?;

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_keystore_write)(&mut f, key_store)
    })
}

pub fn get_cmpa_data(
    data: &mut [u32],
    offset: u32,
    len: u32,
) -> Result<(), FlashStatus> {
    assert!(len <= (data.len() as u32));

    let mut f: FlashConfig = Default::default();
    f.mode_config.sys_freq_in_mhz = get_system_clock_speed_mhz();

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .flash_init)(&mut f)
    })?;

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_init)(&mut f)
    })?;

    handle_flash_status(unsafe {
        (bootloader_tree()
            .flash_driver
            .version1_flash_driver
            .ffr_get_customer_data)(
            &mut f, data.as_mut_ptr(), offset, len
        )
    })
}

// Keep this as a sample function for now
pub fn get_bootloader_version() -> u32 {
    let version = &bootloader_tree().version;
    (version.bugfix as u32)
        | ((version.minor as u32) << 8)
        | ((version.major as u32) << 16)
        | ((version.name as u32) << 24)
}
