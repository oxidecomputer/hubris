// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Functions for writing to flash for updates
//
// This driver is intended to carry as little state as possible. Most of the
// heavy work and decision making should be handled in other tasks.
#![no_std]
#![no_main]

use core::convert::Infallible;
use core::mem::MaybeUninit;
use drv_lpc55_flash::BYTES_PER_FLASH_PAGE;
use drv_lpc55_update_api::{
    RawCabooseError, RotBootInfo, RotPage, SlotId, SwitchDuration, UpdateTarget,
};
use drv_update_api::UpdateError;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};
use stage0_handoff::{
    HandoffData, HandoffDataLoadError, ImageVersion, RotBootState,
};
use userlib::*;
use zerocopy::{AsBytes, FromBytes};

// We shouldn't actually dereference these. The types are not correct.
// They are just here to allow a mechanism for getting the addresses.
extern "C" {
    static __IMAGE_A_BASE: [u32; 0];
    static __IMAGE_B_BASE: [u32; 0];
    static __IMAGE_STAGE0_BASE: [u32; 0];
    static __IMAGE_A_END: [u32; 0];
    static __IMAGE_B_END: [u32; 0];
    static __IMAGE_STAGE0_END: [u32; 0];

    static __this_image: [u32; 0];
}

#[used]
#[link_section = ".bootstate"]
static BOOTSTATE: MaybeUninit<[u8; 0x1000]> = MaybeUninit::uninit();

#[derive(PartialEq)]
enum UpdateState {
    NoUpdate,
    InProgress,
    Finished,
}

// Note that we could cache the full stage0 image before flashing it.
// That would reduce our time window of having a partially written stage0.
struct ServerImpl<'a> {
    header_block: Option<[u8; BLOCK_SIZE_BYTES]>,
    state: UpdateState,
    image: Option<UpdateTarget>,

    flash: drv_lpc55_flash::Flash<'a>,
    hashcrypt: &'a lpc55_pac::hashcrypt::RegisterBlock,
    syscon: drv_lpc55_syscon_api::Syscon,
}

// TODO: This is the size of the vector table on the LPC55. We should
// probably  get it from somewhere else directly.
const MAGIC_OFFSET: usize = 0x130;
const RESET_VECTOR_OFFSET: usize = 4;

const BLOCK_SIZE_BYTES: usize = BYTES_PER_FLASH_PAGE;

const MAX_LEASE: usize = 1024;
const HEADER_BLOCK: usize = 0;

const CMPA_FLASH_WORD: u32 = 0x9E40;
const CFPA_PING_FLASH_WORD: u32 = 0x9E00;
const CFPA_PONG_FLASH_WORD: u32 = 0x9E20;
const CFPA_SCRATCH_FLASH_WORD: u32 = 0x9DE0;
const CFPA_SCRATCH_FLASH_ADDR: u32 = CFPA_SCRATCH_FLASH_WORD << 4;
const BOOT_PREFERENCE_FLASH_WORD_OFFSET: u32 = 0x10;

#[derive(PartialEq)]
enum CfpaPage {
    Active,
    Inactive,
}

impl idl::InOrderUpdateImpl for ServerImpl<'_> {
    fn prep_image_update(
        &mut self,
        _: &RecvMessage,
        image_type: UpdateTarget,
    ) -> Result<(), RequestError<UpdateError>> {
        // The LPC55 doesn't have an easily accessible mass erase mechanism
        // so this is just bookkeeping
        match self.state {
            UpdateState::InProgress => {
                return Err(UpdateError::UpdateInProgress.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::NoUpdate => (),
        }

        self.image = Some(image_type);
        self.state = UpdateState::InProgress;
        Ok(())
    }

    fn abort_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress | UpdateState::NoUpdate => (),
        }

        self.state = UpdateState::NoUpdate;
        Ok(())
    }

    fn write_one_block(
        &mut self,
        _: &RecvMessage,
        block_num: usize,
        block: LenLimit<Leased<R, [u8]>, MAX_LEASE>,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::NoUpdate => {
                return Err(UpdateError::UpdateNotStarted.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress => (),
        }

        let len = block.len();

        // The max lease length is longer than our block size, double
        // check that here. We share the API with other targets and there isn't
        // a nice way to define the lease length based on a constant.
        if len > BLOCK_SIZE_BYTES {
            return Err(UpdateError::BadLength.into());
        }

        // Match the behvaior of the CMSIS flash driver where erased bytes are
        // read as 0xff so the image is padded with 0xff
        const ERASE_BYTE: u8 = 0xff;
        let mut flash_page = [ERASE_BYTE; BLOCK_SIZE_BYTES];
        let target = self.image.unwrap_lite();

        if block_num == HEADER_BLOCK {
            let header_block =
                self.header_block.get_or_insert([0u8; BLOCK_SIZE_BYTES]);
            block
                .read_range(0..len, &mut header_block[..])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            header_block[len..].fill(ERASE_BYTE);
            if let Err(e) = validate_header_block(target, header_block) {
                self.header_block = None;
                return Err(e.into());
            }
        } else {
            // The header block is currently block 0. We should ensure
            // we've seen and cached it before proceeding with other
            // blocks. Otherwise, we won't be able to complete the update in
            // `finish_image_update`.
            if self.header_block.is_none() {
                return Err(UpdateError::MissingHeaderBlock.into());
            }
            block
                .read_range(0..len, &mut flash_page)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

            flash_page[len..].fill(ERASE_BYTE);
        }

        do_block_write(&mut self.flash, target, block_num, &flash_page)?;

        Ok(())
    }

    fn finish_image_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        match self.state {
            UpdateState::NoUpdate => {
                return Err(UpdateError::UpdateNotStarted.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress => (),
        }

        if self.header_block.is_none() {
            return Err(UpdateError::MissingHeaderBlock.into());
        }

        do_block_write(
            &mut self.flash,
            self.image.unwrap_lite(),
            HEADER_BLOCK,
            self.header_block.as_ref().unwrap_lite(),
        )?;

        self.state = UpdateState::Finished;
        self.image = None;
        Ok(())
    }

    fn block_size(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<UpdateError>> {
        Ok(BLOCK_SIZE_BYTES)
    }

    // TODO(AJS): Remove this in favor of `status`, once SP code is updated.
    // This has ripple effects up thorugh control-plane-agent.
    fn current_version(
        &mut self,
        _: &RecvMessage,
    ) -> Result<ImageVersion, RequestError<Infallible>> {
        Ok(ImageVersion {
            epoch: HUBRIS_BUILD_EPOCH,
            version: HUBRIS_BUILD_VERSION,
        })
    }

    fn status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<RotBootState, RequestError<HandoffDataLoadError>> {
        // Safety: Data is published by stage0
        let addr = unsafe { BOOTSTATE.assume_init_ref() };
        RotBootState::load_from_addr(addr).map_err(|e| e.into())
    }

    fn rot_boot_info(
        &mut self,
        _: &RecvMessage,
    ) -> Result<RotBootInfo, RequestError<UpdateError>> {
        // Safety: Data is published by pre-kernel main
        let addr = unsafe { BOOTSTATE.assume_init_ref() };
        let boot_state = RotBootState::load_from_addr(addr)
            .map_err(|_| UpdateError::MissingHandoffData)?;
        let (
            persistent_boot_preference,
            pending_persistent_boot_preference,
            transient_boot_preference,
        ) = self.boot_preferences()?;

        let info = RotBootInfo {
            active: boot_state.active.into(),
            persistent_boot_preference,
            pending_persistent_boot_preference,
            transient_boot_preference,
            slot_a_sha3_256_digest: boot_state.a.map(|details| details.digest),
            slot_b_sha3_256_digest: boot_state.b.map(|details| details.digest),
        };
        Ok(info)
    }

    fn read_raw_caboose(
        &mut self,
        _msg: &RecvMessage,
        slot: SlotId,
        offset: u32,
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), RequestError<RawCabooseError>> {
        let caboose = caboose_slice(&self.flash, slot)?;
        if offset as usize + data.len() > caboose.len() {
            return Err(RawCabooseError::InvalidRead.into());
        }
        copy_from_caboose_chunk(
            &self.flash,
            caboose,
            offset..offset + data.len() as u32,
            data,
        )
    }

    fn caboose_size(
        &mut self,
        _: &RecvMessage,
        slot: SlotId,
    ) -> Result<u32, RequestError<RawCabooseError>> {
        let caboose = caboose_slice(&self.flash, slot)?;
        Ok(caboose.end - caboose.start)
    }

    fn switch_default_image(
        &mut self,
        _: &userlib::RecvMessage,
        slot: SlotId,
        duration: SwitchDuration,
    ) -> Result<(), RequestError<UpdateError>> {
        match duration {
            SwitchDuration::Once => {
                // TODO deposit command token into buffer
                return Err(UpdateError::NotImplemented.into());
            }
            SwitchDuration::Forever => {
                // Locate and return the authoritative CFPA flash word number
                // and the CFPA version for that flash number.
                //
                // There are two "official" copies of the CFPA, referred to as
                // ping and pong. One of them will supercede the other, based on
                // a monotonic version field at offset 4. We'll take the
                // contents of whichever one is most recent, alter them, and
                // then write them into the _third_ copy, called the scratch
                // page.
                //
                // At reset, the boot ROM will inspect the scratch page, check
                // invariants, and copy it to overwrite the older of the ping
                // and pong pages if it approves.
                //
                // That means you can apply this operation several times before
                // resetting without burning many monotonic versions, if you
                // want to do that for some reason.
                //
                // The addresses of these pages are as follows (see Figure 13,
                // "Protected Flash Region," in UM11126 rev 2.4, or the NXP
                // flash layout spreadsheet):
                //
                // Page     Addr        16-byte word number
                // Scratch  0x9_DE00    0x9DE0
                // Ping     0x9_E000    0x9E00
                // Pong     0x9_E200    0x9E20
                let (cfpa_word_number, _) =
                    self.cfpa_word_number_and_version(CfpaPage::Active)?;

                // Read current CFPA contents.
                let mut cfpa = [[0u32; 4]; 512 / 16];
                indirect_flash_read_words(
                    &self.flash,
                    cfpa_word_number,
                    &mut cfpa,
                )?;

                // Alter the boot setting, if it needs changing. The boot
                // setting (per RFD 374) is in the lowest bit of the 32-bit word
                // starting at (byte) offset 0x100. This is flash word offset
                // 0x10.
                //
                // Leave remaining bits undisturbed; they are currently
                // reserved.
                let offset = BOOT_PREFERENCE_FLASH_WORD_OFFSET as usize;
                let bit = cfpa[offset][0] & 1;
                let new_bit = u32::from(slot != SlotId::A);
                if bit == new_bit {
                    // No need to write the CFPA if it's unchanged
                    return Ok(());
                }
                cfpa[offset][0] &= !1;
                cfpa[offset][0] |= new_bit;
                // Increment the monotonic version. The manual doesn't specify
                // how the version numbers are compared or what happens if they
                // wrap, so, we'll treat wrapping as an error and report it for
                // now. (Note that getting this version to wrap _should_ require
                // more write cycles than the flash can take.)
                let new_version =
                    cfpa[0][1].checked_add(1).ok_or(UpdateError::SecureErr)?;
                cfpa[0][1] = new_version;
                // The last two flash words are a SHA256 hash of the preceding
                // data. This means we need to compute a SHA256 hash of the
                // preceding data -- meaning flash words 0 thru 29 inclusive.
                let cfpa_hash = {
                    // We leave the hashcrypt unit in reset when unused,
                    // starting in the `main` function, so we only need to bring
                    // it _out of_ reset here.
                    self.syscon
                        .leave_reset(drv_lpc55_syscon_api::Peripheral::HashAes);
                    let mut h = drv_lpc55_sha256::Hasher::begin(
                        self.hashcrypt,
                        notifications::HASHCRYPT_IRQ_MASK,
                    );
                    for chunk in &cfpa[..30] {
                        h.update(chunk, 0);
                    }
                    let hash = h.finish();

                    // Put it back.
                    self.syscon
                        .enter_reset(drv_lpc55_syscon_api::Peripheral::HashAes);

                    hash
                };
                cfpa[30] = cfpa_hash[..4].try_into().unwrap_lite();
                cfpa[31] = cfpa_hash[4..].try_into().unwrap_lite();

                // Recast that as a page-sized byte array because that's what
                // the update side of the machinery wants. The try_into on the
                // second line can't fail at runtime, but there's no good
                // support for casting between fixed-size arrays in zerocopy
                // yet.
                let cfpa_bytes: &[u8] = cfpa.as_bytes();
                let cfpa_bytes: &[u8; BLOCK_SIZE_BYTES] =
                    cfpa_bytes.try_into().unwrap_lite();

                // Erase and program the scratch page. Note that because the
                // scratch page is _not_ the authoritative copy, and because the
                // ROM will check its contents before making it authoritative,
                // we can fail during this operation without corrupting anything
                // permanent. Yay!
                //
                self.flash
                    .write_page(
                        CFPA_SCRATCH_FLASH_ADDR,
                        cfpa_bytes,
                        wait_for_flash_interrupt,
                    )
                    .map_err(|_| UpdateError::FlashError)?;
            }
        }

        Ok(())
    }

    /// Reset.
    fn reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        if self.state == UpdateState::InProgress {
            return Err(UpdateError::UpdateInProgress.into());
        }
        self.syscon.chip_reset();
        panic!()
    }

    fn read_rot_page(
        &mut self,
        _: &RecvMessage,
        page: RotPage,
        dest: LenLimit<Leased<W, [u8]>, BYTES_PER_FLASH_PAGE>,
    ) -> Result<(), RequestError<UpdateError>> {
        let start_addr = match page {
            RotPage::Cmpa => CMPA_FLASH_WORD << 4,
            RotPage::CfpaScratch => CFPA_SCRATCH_FLASH_ADDR,
            RotPage::CfpaActive => {
                let (cfpa_word, _) =
                    self.cfpa_word_number_and_version(CfpaPage::Active)?;
                cfpa_word << 4
            }
            RotPage::CfpaInactive => {
                let (cfpa_word, _) =
                    self.cfpa_word_number_and_version(CfpaPage::Inactive)?;
                cfpa_word << 4
            }
        };

        const PAGE_SIZE: u32 = BYTES_PER_FLASH_PAGE as u32;

        copy_from_flash_range(
            &self.flash,
            start_addr..(start_addr + PAGE_SIZE),
            0..PAGE_SIZE,
            dest,
        )?;
        Ok(())
    }
}

impl ServerImpl<'_> {
    fn cfpa_word_number_and_version(
        &mut self,
        page: CfpaPage,
    ) -> Result<(u32, u32), UpdateError> {
        // Read the two versions. We do this with smaller buffers so
        // we don't need 2x 512B buffers to read the entire CFPAs.
        let mut ping_header = [0u32; 4];
        let mut pong_header = [0u32; 4];

        indirect_flash_read_words(
            &self.flash,
            CFPA_PING_FLASH_WORD,
            core::slice::from_mut(&mut ping_header),
        )?;
        indirect_flash_read_words(
            &self.flash,
            CFPA_PONG_FLASH_WORD,
            core::slice::from_mut(&mut pong_header),
        )?;

        let val =
            if ping_header[1] >= pong_header[1] && page == CfpaPage::Active {
                (CFPA_PING_FLASH_WORD, ping_header[1])
            } else {
                (CFPA_PONG_FLASH_WORD, pong_header[1])
            };

        Ok(val)
    }

    // Return the persistent and transient boot preferences
    fn boot_preferences(
        &mut self,
    ) -> Result<(SlotId, Option<SlotId>, Option<SlotId>), UpdateError> {
        let (cfpa_word_number, cfpa_version) =
            self.cfpa_word_number_and_version(CfpaPage::Active)?;

        // Read the authoritative boot selection
        let boot_selection_word_number =
            cfpa_word_number + BOOT_PREFERENCE_FLASH_WORD_OFFSET;
        let mut boot_selection_word = [0u32; 4];
        indirect_flash_read_words(
            &self.flash,
            boot_selection_word_number,
            core::slice::from_mut(&mut boot_selection_word),
        )?;

        // Check the authoritative persistent boot selection bit
        let persistent_boot_preference =
            boot_preference_from_flash_word(&boot_selection_word);

        // Read the scratch boot version, which may be erased
        let mut scratch_header = [0u32; 4];
        let scratch_header = match indirect_flash_read_words(
            &self.flash,
            CFPA_SCRATCH_FLASH_WORD,
            core::slice::from_mut(&mut scratch_header),
        ) {
            Ok(()) => Some(scratch_header),
            Err(UpdateError::EccDoubleErr) => None,
            Err(e) => return Err(e),
        };

        // We only have a pending preference if the scratch CFPA page is newer
        // than the authoritative page.
        let pending_persistent_boot_preference =
            if scratch_header.map(|s| s[1] > cfpa_version).unwrap_or(false) {
                // Read the scratch boot selection
                let scratch_boot_selection_word_number =
                    CFPA_SCRATCH_FLASH_WORD + BOOT_PREFERENCE_FLASH_WORD_OFFSET;
                let mut scratch_boot_selection_word = [0u32; 4];
                indirect_flash_read_words(
                    &self.flash,
                    scratch_boot_selection_word_number,
                    core::slice::from_mut(&mut scratch_boot_selection_word),
                )?;
                Some(boot_preference_from_flash_word(
                    &scratch_boot_selection_word,
                ))
            } else {
                None
            };

        // We only support persistent override at this point
        // We need to read the magic ram value to fill this in.
        let transient_boot_preference = None;

        Ok((
            persistent_boot_preference,
            pending_persistent_boot_preference,
            transient_boot_preference,
        ))
    }
}

// Return the preferred slot to boot from for a given CFPA boot selection
// flash word.
//
// This matches the logic in bootleby
fn boot_preference_from_flash_word(flash_word: &[u32; 4]) -> SlotId {
    if flash_word[0] & 1 == 0 {
        SlotId::A
    } else {
        SlotId::B
    }
}

/// Reads an arbitrary contiguous set of flash words from flash, indirectly,
/// using the flash controller interface. This allows access to sections of
/// flash that are not direct-mapped into our task's memory, saving MPU regions.
///
/// `flash_word_number` is (as its name suggests) a _word number,_ a 0-based
/// index of a 16-byte word within flash where reading should begin.
///
/// `output` implies the length to read.
///
/// The main way this can fail is by ECC error; currently, if this occurs, no
/// feedback is provided as to _where_ in the region the error occurred. We may
/// wish to fix this.
///
/// This API produces flash words in the form of `[u32; 4]`, because that's how
/// the hardware produces them. Elements of the array are in ascending address
/// order when the flash is viewed as bytes. The easiest way to view the
/// corresponding block of 16 bytes is using `zerocopy::AsBytes` to reinterpret
/// the array in place.
fn indirect_flash_read_words(
    flash: &drv_lpc55_flash::Flash<'_>,
    flash_word_number: u32,
    output: &mut [[u32; 4]],
) -> Result<(), UpdateError> {
    use drv_lpc55_flash::ReadError;

    for (wn, dest) in (flash_word_number..).zip(output) {
        flash.start_read(wn);
        loop {
            // Reading is relatively fast; this loop will most likely not sleep,
            // most of the time.
            if let Some(result) = flash.poll_read_result() {
                match result {
                    Ok(data) => {
                        *dest = data;
                        break;
                    }
                    Err(ReadError::IllegalOperation) => {
                        return Err(UpdateError::FlashIllegalRead);
                    }
                    Err(ReadError::Ecc) => {
                        return Err(UpdateError::EccDoubleErr);
                    }
                    Err(ReadError::Fail) => {
                        return Err(UpdateError::FlashReadFail);
                    }
                }
            }

            // But just in case it needs to:

            flash.enable_interrupt_sources();
            sys_irq_control(notifications::FLASH_IRQ_MASK, true);
            // RECV from the kernel cannot produce an error, so ignore it.
            let _ = sys_recv_closed(
                &mut [],
                notifications::FLASH_IRQ_MASK,
                TaskId::KERNEL,
            );
            flash.disable_interrupt_sources();
        }
    }

    Ok(())
}

/// Reads an arbitrary contiguous set of bytes from flash, indirectly,
/// using the flash controller interface. This allows access to sections of
/// flash that are not direct-mapped into our task's memory, saving MPU regions.
///
/// Under the hood, this calls into `indirect_flash_read_words` and reads
/// 128-byte chunks at a time.
fn indirect_flash_read(
    flash: &drv_lpc55_flash::Flash<'_>,
    mut addr: u32,
    mut output: &mut [u8],
) -> Result<(), UpdateError> {
    while !output.is_empty() {
        // Convert from memory (byte) address to word address, per comments in
        // `lpc55_flash` driver.
        let word = (addr / 16) & ((1 << 18) - 1);

        // Read 128 bytes into a local buffer
        let mut buf = [0u32; 4];
        indirect_flash_read_words(
            flash,
            word,
            core::slice::from_mut(&mut buf),
        )?;

        // If we rounded down to snap to a word boundary, then only a subset of
        // the data is valid, so adjust here.
        let chunk = &buf.as_bytes()[(addr - (word * 16)) as usize..];

        // Since we always read 128 bytes at a time, we may have over-read
        let count = chunk.len().min(output.len());

        // Copy data into our output buffer
        output[..count].copy_from_slice(&chunk[..count]);

        // Adjust everything and continue
        output = &mut output[count..];
        addr = addr
            .checked_add(count as u32)
            .ok_or(UpdateError::OutOfBounds)?;
    }
    Ok(())
}

// Perform some sanity checking on the header block.
fn validate_header_block(
    target: UpdateTarget,
    block: &[u8; BLOCK_SIZE_BYTES],
) -> Result<(), UpdateError> {
    // TODO: Do some actual checks for stage0. This will likely change
    // with Cliff's bootloader.
    if target == UpdateTarget::Bootloader {
        return Ok(());
    }

    // This part aliases flash in two positions that differ in bit 28. To allow
    // for either position to be used in new images, we clear bit 28 in all of
    // the numbers used for comparison below, by ANDing them with this mask:
    const ADDRMASK: u32 = !(1 << 28);

    let reset_vector = u32::from_le_bytes(
        block[RESET_VECTOR_OFFSET..][..4].try_into().unwrap_lite(),
    ) & ADDRMASK;
    let a_base = unsafe { __IMAGE_A_BASE.as_ptr() } as u32 & ADDRMASK;
    let b_base = unsafe { __IMAGE_B_BASE.as_ptr() } as u32 & ADDRMASK;
    let stage0_base = unsafe { __IMAGE_STAGE0_BASE.as_ptr() } as u32 & ADDRMASK;
    let a_end = unsafe { __IMAGE_A_END.as_ptr() } as u32 & ADDRMASK;
    let b_end = unsafe { __IMAGE_B_END.as_ptr() } as u32 & ADDRMASK;
    let stage0_end = unsafe { __IMAGE_STAGE0_END.as_ptr() } as u32 & ADDRMASK;

    // Ensure the image is destined for the right target
    let valid = match target {
        UpdateTarget::ImageA => (a_base..a_end).contains(&reset_vector),
        UpdateTarget::ImageB => (b_base..b_end).contains(&reset_vector),
        UpdateTarget::Bootloader => {
            (stage0_base..stage0_end).contains(&reset_vector)
        }
        UpdateTarget::_Reserved => false,
    };
    if !valid {
        return Err(UpdateError::InvalidHeaderBlock);
    }

    // Ensure the MAGIC is correct
    let magic =
        u32::from_le_bytes(block[MAGIC_OFFSET..][..4].try_into().unwrap_lite());
    if magic != abi::HEADER_MAGIC {
        return Err(UpdateError::InvalidHeaderBlock);
    }

    Ok(())
}

/// Performs an erase-write sequence to a single page within a given target
/// image.
fn do_block_write(
    flash: &mut drv_lpc55_flash::Flash<'_>,
    img: UpdateTarget,
    block_num: usize,
    flash_page: &[u8; BLOCK_SIZE_BYTES],
) -> Result<(), UpdateError> {
    // The update.idol definition uses usize; our hardware uses u32; convert
    // here so we don't have to cast everywhere.
    let page_num = block_num as u32;

    // Can only update opposite image
    if same_image(img) {
        return Err(UpdateError::RunningImage);
    }

    let write_addr = match target_addr(img, page_num) {
        Some(addr) => addr,
        None => return Err(UpdateError::OutOfBounds),
    };

    flash
        .write_page(write_addr, flash_page, wait_for_flash_interrupt)
        .map_err(|_| UpdateError::FlashError)
}

fn wait_for_flash_interrupt() {
    sys_irq_control(notifications::FLASH_IRQ_MASK, true);
    // RECV from the kernel cannot produce an error, so ignore it.
    let _ =
        sys_recv_closed(&mut [], notifications::FLASH_IRQ_MASK, TaskId::KERNEL);
}

fn same_image(which: UpdateTarget) -> bool {
    get_base(which) == unsafe { __this_image.as_ptr() } as u32
}

/// Returns the byte address of the first byte of the given flash target slot,
/// or panics if you're holding it wrong
fn get_base(which: UpdateTarget) -> u32 {
    (match which {
        UpdateTarget::ImageA => unsafe { __IMAGE_A_BASE.as_ptr() },
        UpdateTarget::ImageB => unsafe { __IMAGE_B_BASE.as_ptr() },
        UpdateTarget::Bootloader => unsafe { __IMAGE_STAGE0_BASE.as_ptr() },
        UpdateTarget::_Reserved => panic!(),
    }) as u32
}

fn get_end(which: UpdateTarget) -> u32 {
    (match which {
        UpdateTarget::ImageA => unsafe { __IMAGE_A_END.as_ptr() },
        UpdateTarget::ImageB => unsafe { __IMAGE_B_END.as_ptr() },
        UpdateTarget::Bootloader => unsafe { __IMAGE_STAGE0_END.as_ptr() },
        UpdateTarget::_Reserved => panic!(),
    }) as u32
}

/// Computes the byte address of the first byte in a particular (slot, page)
/// combination.
///
/// `image_target` designates the flash slot and must be `ImageA`, `ImageB`, or
/// `Bootloader`, despite containing other variants.  All other choices will
/// panic. (TODO: fix this when time permits.)
///
/// `page_num` designates a flash page (called a block elsewhere in this file, a
/// 512B unit) within the flash slot. If the page is out range for the target
/// slot, returns `None`.
fn target_addr(image_target: UpdateTarget, page_num: u32) -> Option<u32> {
    let base = get_base(image_target);

    // This is safely calculating addr = base + page_num * PAGE_SIZE
    let addr = page_num
        .checked_mul(BLOCK_SIZE_BYTES as u32)?
        .checked_add(base)?;

    // check addr + PAGE_SIZE <= end
    if addr.checked_add(BLOCK_SIZE_BYTES as u32)? > get_end(image_target) {
        return None;
    }

    Some(addr)
}

/// Finds the memory range which contains the caboose for the given slot
///
/// This implementation has similar logic to the one in `stm32h7-update-server`,
/// but uses indirect reads instead of mapping the alternate bank into flash.
fn caboose_slice(
    flash: &drv_lpc55_flash::Flash<'_>,
    slot: SlotId,
) -> Result<core::ops::Range<u32>, RawCabooseError> {
    // SAFETY: these symbols are populated by the linker
    let (image_start, image_region_end) = unsafe {
        match slot {
            SlotId::A => (
                __IMAGE_A_BASE.as_ptr() as u32,
                __IMAGE_A_END.as_ptr() as u32,
            ),
            SlotId::B => (
                __IMAGE_B_BASE.as_ptr() as u32,
                __IMAGE_B_END.as_ptr() as u32,
            ),
        }
    };

    // If all is going according to plan, there will be a valid Hubris image
    // flashed into the other slot, delimited by `__IMAGE_A/B_BASE` and
    // `__IMAGE_A/B_END` (which are symbols injected by the linker).
    //
    // We'll first want to read the image header, which is at a fixed
    // location at the end of the vector table.  The length of the vector
    // table is fixed in hardware, so this should never change.
    const HEADER_OFFSET: u32 = 0x130;
    let mut header = ImageHeader::new_zeroed();

    indirect_flash_read(
        flash,
        image_start + HEADER_OFFSET,
        header.as_bytes_mut(),
    )
    .map_err(|_| RawCabooseError::ReadFailed)?;
    if header.magic != HEADER_MAGIC {
        return Err(RawCabooseError::NoImageHeader);
    }

    // Calculate where the image header implies that the image should end
    //
    // This is a one-past-the-end value.
    let image_end = image_start + header.total_image_len;

    // Then, check that value against the BANK2 bounds.
    //
    // SAFETY: populated by the linker, so this should be valid
    if image_end > image_region_end {
        return Err(RawCabooseError::MissingCaboose);
    }

    // By construction, the last word of the caboose is its size as a `u32`
    let mut caboose_size = 0u32;
    indirect_flash_read(flash, image_end - 4, caboose_size.as_bytes_mut())
        .map_err(|_| RawCabooseError::ReadFailed)?;

    let caboose_start = image_end.saturating_sub(caboose_size);
    let caboose_range = if caboose_start < image_start {
        // This branch will be encountered if there's no caboose, because
        // then the nominal caboose size will be 0xFFFFFFFF, which will send
        // us out of the bank2 region.
        return Err(RawCabooseError::MissingCaboose);
    } else {
        // SAFETY: we know this pointer is within the programmed flash region,
        // since it's checked above.
        let mut v = 0u32;
        indirect_flash_read(flash, caboose_start, v.as_bytes_mut())
            .map_err(|_| RawCabooseError::ReadFailed)?;
        if v == CABOOSE_MAGIC {
            caboose_start + 4..image_end - 4
        } else {
            return Err(RawCabooseError::MissingCaboose);
        }
    };
    Ok(caboose_range)
}

fn copy_from_caboose_chunk(
    flash: &drv_lpc55_flash::Flash<'_>,
    caboose: core::ops::Range<u32>,
    pos: core::ops::Range<u32>,
    data: Leased<idol_runtime::W, [u8]>,
) -> Result<(), RequestError<RawCabooseError>> {
    // Early exit if the caller didn't provide enough space in the lease
    let mut remaining = pos.end - pos.start;
    if remaining as usize > data.len() {
        return Err(RequestError::Fail(ClientError::BadLease))?;
    }

    const BUF_SIZE: usize = 128;
    let mut offset = 0;
    let mut buf = [0u8; BUF_SIZE];
    while remaining > 0 {
        let count = remaining.min(buf.len() as u32);
        let buf = &mut buf[..count as usize];
        indirect_flash_read(flash, caboose.start + pos.start + offset, buf)
            .map_err(|_| RequestError::from(RawCabooseError::ReadFailed))?;
        data.write_range(offset as usize..(offset + count) as usize, buf)
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        offset += count;
        remaining -= count;
    }
    Ok(())
}

fn copy_from_flash_range(
    flash: &drv_lpc55_flash::Flash<'_>,
    range: core::ops::Range<u32>,
    pos: core::ops::Range<u32>,
    data: LenLimit<Leased<W, [u8]>, BYTES_PER_FLASH_PAGE>,
) -> Result<(), RequestError<UpdateError>> {
    // Early exit if the caller didn't provide enough space in the lease
    let mut remaining = pos.end - pos.start;
    if remaining as usize > data.len() {
        return Err(RequestError::Fail(ClientError::BadLease))?;
    }

    const BUF_SIZE: usize = 128;
    let mut offset = 0;
    let mut buf = [0u8; BUF_SIZE];
    while remaining > 0 {
        let count = remaining.min(buf.len() as u32);
        let buf = &mut buf[..count as usize];
        indirect_flash_read(flash, range.start + pos.start + offset, buf)?;
        data.write_range(offset as usize..(offset + count) as usize, buf)
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        offset += count;
        remaining -= count;
    }
    Ok(())
}

task_slot!(SYSCON, syscon);
task_slot!(JEFE, jefe);

#[export_name = "main"]
fn main() -> ! {
    let syscon = drv_lpc55_syscon_api::Syscon::from(SYSCON.get_task_id());

    // Go ahead and put the HASHCRYPT unit into reset.
    syscon.enter_reset(drv_lpc55_syscon_api::Peripheral::HashAes);
    let mut server = ServerImpl {
        header_block: None,
        state: UpdateState::NoUpdate,
        image: None,

        flash: drv_lpc55_flash::Flash::new(unsafe {
            &*lpc55_pac::FLASH::ptr()
        }),
        hashcrypt: unsafe { &*lpc55_pac::HASHCRYPT::ptr() },
        syscon,
    };
    let mut incoming = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

include!(concat!(env!("OUT_DIR"), "/consts.rs"));
include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
mod idl {
    use super::{
        HandoffDataLoadError, ImageVersion, RawCabooseError, RotBootInfo,
        RotPage, SlotId, SwitchDuration, UpdateTarget,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
