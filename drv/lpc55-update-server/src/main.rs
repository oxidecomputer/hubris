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
use core::ops::Range;
use drv_lpc55_flash::{BYTES_PER_FLASH_PAGE, BYTES_PER_FLASH_WORD};
use drv_lpc55_update_api::{
    Fwid, RawCabooseError, RotBootInfo, RotBootInfoV2, RotComponent, RotPage,
    SlotId, SwitchDuration, UpdateTarget, VersionedRotBootInfo,
};
use drv_update_api::UpdateError;
use hex_literal::hex;
use idol_runtime::{
    ClientError, Leased, LenLimit, NotificationHandler, RequestError, R, W,
};
use ringbuf::*;
use sha3::{Digest, Sha3_256};
use stage0_handoff::{
    HandoffData, HandoffDataLoadError, ImageVersion, RotBootState,
    RotBootStateV2,
};
use userlib::*;
use zerocopy::{FromZeros, IntoBytes};

mod images;
use crate::images::*;

const U32_SIZE: u32 = core::mem::size_of::<u32>() as u32;
const PAGE_SIZE: u32 = BYTES_PER_FLASH_PAGE as u32;

#[used]
#[link_section = ".bootstate"]
static BOOTSTATE: MaybeUninit<[u8; 0x1000]> = MaybeUninit::uninit();

#[used]
#[link_section = ".transient_override"]
static mut TRANSIENT_OVERRIDE: MaybeUninit<[u8; 32]> = MaybeUninit::uninit();

#[derive(Copy, Clone, PartialEq)]
enum UpdateState {
    NoUpdate,
    InProgress,
    Finished,
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    State(UpdateState),
    Prep(RotComponent, SlotId),
}

ringbuf!(Trace, 16, Trace::None);

/// FW_CACHE_MAX accomodates the largest production
/// bootloader image while allowing some room for growth.
///
/// NOTE: The erase/flash of stage0 can be interrupted by a power failure or
/// reset.
/// The LPC55S69 ROM offers no A/B image backup. Therefore the partially
/// updated stage0 contents would fail the boot-time signature check thus
/// rendering the RoT inoperable.
///
/// The full stage0 image is cached before flashing to reduce that window
/// of vulnerability.
///
/// While the stage0next flash slot can be updated in parallel, not more than
/// one RoT should have its stage0 updated (persist operation) at any
/// one time.
///
/// No addition RoT stage0 slots should be updated until a previous failure
/// can be diagnosed.
///
const FW_CACHE_MAX: usize = 8192_usize;
struct ServerImpl<'a> {
    header_block: Option<[u8; BLOCK_SIZE_BYTES]>,
    state: UpdateState,
    image: Option<(RotComponent, SlotId)>,

    flash: drv_lpc55_flash::Flash<'a>,
    hashcrypt: &'a lpc55_pac::hashcrypt::RegisterBlock,
    syscon: drv_lpc55_syscon_api::Syscon,

    // Used to enforce sequential writes from the control plane.
    next_block: Option<usize>,
    // Keep the fw cache 32-bit aligned to make NXP header access easier.
    fw_cache: &'a mut [u32; FW_CACHE_MAX / core::mem::size_of::<u32>()],
}

const BLOCK_SIZE_BYTES: usize = BYTES_PER_FLASH_PAGE;

const MAX_LEASE: usize = 1024;

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
        msg: &RecvMessage,
        image_type: UpdateTarget,
    ) -> Result<(), RequestError<UpdateError>> {
        let (component, slot) = match image_type {
            UpdateTarget::ImageA => (RotComponent::Hubris, SlotId::A),
            UpdateTarget::ImageB => (RotComponent::Hubris, SlotId::B),
            UpdateTarget::Bootloader => (RotComponent::Stage0, SlotId::B),
            _ => return Err(UpdateError::InvalidSlotIdForOperation.into()),
        };
        self.component_prep_image_update(msg, component, slot)
    }

    fn abort_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::State(self.state));
        match self.state {
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress | UpdateState::NoUpdate => (),
        }

        self.state = UpdateState::NoUpdate;
        ringbuf_entry!(Trace::State(self.state));
        self.next_block = None;
        self.fw_cache.fill(0);
        Ok(())
    }

    fn write_one_block(
        &mut self,
        _: &RecvMessage,
        block_num: usize,
        block: LenLimit<Leased<R, [u8]>, MAX_LEASE>,
    ) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::State(self.state));
        match self.state {
            UpdateState::NoUpdate => {
                return Err(UpdateError::UpdateNotStarted.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress => (),
        }

        // Check that blocks are delivered in order.
        let next = self.next_block.get_or_insert(0);
        if block_num != *next {
            return Err(UpdateError::BlockOutOfOrder.into());
        }
        *next += 1;

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
        let (component, slot) = self.image.unwrap_lite();

        if block_num == HEADER_BLOCK {
            let header_block =
                self.header_block.get_or_insert([0u8; BLOCK_SIZE_BYTES]);
            block
                .read_range(0..len, &mut header_block[..])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            header_block[len..].fill(ERASE_BYTE);
            if let Err(e) = validate_header_block(component, slot, header_block)
            {
                self.header_block = None;
                return Err(e.into());
            }
        } else {
            // Block order is enforced above. If we're here then we have
            // seen block zero already.
            block
                .read_range(0..len, &mut flash_page)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

            flash_page[len..].fill(ERASE_BYTE);
        }

        do_block_write(
            &mut self.flash,
            component,
            slot,
            block_num,
            &flash_page,
        )?;

        Ok(())
    }

    fn finish_image_update(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::State(self.state));
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

        // Check for nothing written
        let endblock =
            self.next_block.ok_or(UpdateError::MissingHeaderBlock)?;
        if endblock == 0 {
            // Nothing to do if no data was received.
            return Err(UpdateError::MissingHeaderBlock.into());
        }

        let (component, slot) = self.image.unwrap_lite();
        do_block_write(
            &mut self.flash,
            component,
            slot,
            HEADER_BLOCK,
            self.header_block.as_ref().unwrap_lite(),
        )?;

        // Now erase the unused portion of the flash slot so that
        // flash slot has predictable contents and the FWID for it
        // has some meaning.
        let range = image_range(component, slot).0;
        let erase_start = range.start + (endblock as u32 * PAGE_SIZE);
        self.flash_erase_range(erase_start..range.end)?;
        self.state = UpdateState::Finished;
        ringbuf_entry!(Trace::State(self.state));
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
        bootstate().map(RotBootState::from).map_err(|e| e.into())
    }

    fn rot_boot_info(
        &mut self,
        _: &RecvMessage,
    ) -> Result<RotBootInfo, RequestError<UpdateError>> {
        let boot_state =
            bootstate().map_err(|_| UpdateError::MissingHandoffData)?;
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
            // There is a change in meaning from the original
            // RotBootInfo:
            // Previously, None meant that the image did not
            // validate, but there was no indication of the
            // contents of the flash slot. Now, the FWID (hash
            // of all programmed pages in the slot) is always
            // available. The "valid image in the slot" semantic
            // was not being used and is no longer available in
            // this version of RotBootInfo.
            slot_a_sha3_256_digest: Some(boot_state.a.digest),
            slot_b_sha3_256_digest: Some(boot_state.b.digest),
        };
        Ok(info)
    }

    fn versioned_rot_boot_info(
        &mut self,
        _: &RecvMessage,
        version: u8,
    ) -> Result<VersionedRotBootInfo, RequestError<UpdateError>> {
        let boot_state =
            bootstate().map_err(|_| UpdateError::MissingHandoffData)?;
        let (
            persistent_boot_preference,
            pending_persistent_boot_preference,
            transient_boot_preference,
        ) = self.boot_preferences()?;

        match version {
            // There are deprecated versions
            0 => Err(UpdateError::VersionNotSupported.into()),
            1 => Ok(VersionedRotBootInfo::V1(RotBootInfo {
                active: boot_state.active.into(),
                persistent_boot_preference,
                pending_persistent_boot_preference,
                transient_boot_preference,
                slot_a_sha3_256_digest: Some(boot_state.a.digest),
                slot_b_sha3_256_digest: Some(boot_state.b.digest),
            })),
            // Forward compatibility: If our caller wants a higher
            // version than we know about, return the highest that we
            // do know about.
            // Rollback protection and deprecation of older versions
            // will allow us to eventually remove old implementations.
            _ => Ok(VersionedRotBootInfo::V2(RotBootInfoV2 {
                active: boot_state.active.into(),
                persistent_boot_preference,
                pending_persistent_boot_preference,
                transient_boot_preference,
                slot_a_fwid: Fwid::Sha3_256(boot_state.a.digest),
                slot_b_fwid: Fwid::Sha3_256(boot_state.b.digest),
                stage0_fwid: Fwid::Sha3_256(boot_state.stage0.digest),
                stage0next_fwid: Fwid::Sha3_256(boot_state.stage0next.digest),
                slot_a_status: boot_state.a.status,
                slot_b_status: boot_state.b.status,
                stage0_status: boot_state.stage0.status,
                stage0next_status: boot_state.stage0next.status,
            })),
        }
    }

    fn read_raw_caboose(
        &mut self,
        _msg: &RecvMessage,
        slot: SlotId,
        offset: u32,
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), RequestError<RawCabooseError>> {
        let caboose = caboose_slice(&self.flash, RotComponent::Hubris, slot)?;
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
        let caboose = caboose_slice(&self.flash, RotComponent::Hubris, slot)?;
        Ok(caboose.end - caboose.start)
    }

    fn switch_default_image(
        &mut self,
        _: &userlib::RecvMessage,
        slot: SlotId,
        duration: SwitchDuration,
    ) -> Result<(), RequestError<UpdateError>> {
        self.switch_default_hubris_image(slot, duration)
    }

    /// Reset.
    fn reset(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<UpdateError>> {
        ringbuf_entry!(Trace::State(self.state));
        if self.state == UpdateState::InProgress {
            return Err(UpdateError::UpdateInProgress.into());
        }
        self.syscon.chip_reset();
        unreachable!();
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

        copy_from_flash_range(
            &self.flash,
            start_addr..(start_addr + PAGE_SIZE),
            0..PAGE_SIZE,
            dest,
        )?;
        Ok(())
    }

    fn component_caboose_size(
        &mut self,
        _msg: &userlib::RecvMessage,
        component: RotComponent,
        slot: SlotId,
    ) -> Result<u32, idol_runtime::RequestError<RawCabooseError>> {
        let caboose = caboose_slice(&self.flash, component, slot)?;
        Ok(caboose.end - caboose.start)
    }

    fn component_read_raw_caboose(
        &mut self,
        _msg: &RecvMessage,
        component: RotComponent,
        slot: SlotId,
        offset: u32,
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<RawCabooseError>> {
        let caboose = caboose_slice(&self.flash, component, slot)?;
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

    fn component_prep_image_update(
        &mut self,
        _msg: &userlib::RecvMessage,
        component: RotComponent,
        slot: SlotId,
    ) -> Result<(), RequestError<UpdateError>> {
        // The LPC55 doesn't have an easily accessible mass erase mechanism
        // so this is just bookkeeping
        ringbuf_entry!(Trace::State(self.state));
        ringbuf_entry!(Trace::Prep(component, slot));
        match self.state {
            UpdateState::InProgress => {
                return Err(UpdateError::UpdateInProgress.into())
            }
            UpdateState::Finished | UpdateState::NoUpdate => (),
        }

        self.image = match (component, slot) {
            (RotComponent::Hubris, SlotId::A)
            | (RotComponent::Hubris, SlotId::B)
            | (RotComponent::Stage0, SlotId::B) => Some((component, slot)),
            _ => return Err(UpdateError::InvalidSlotIdForOperation.into()),
        };
        self.state = UpdateState::InProgress;
        ringbuf_entry!(Trace::State(self.state));
        self.next_block = None;
        self.fw_cache.fill(0);
        // The sequence: [update, set transient preference, update] is legal.
        // Clear any stale transient preference before update.
        // Stage0 doesn't support transient override.
        if component == RotComponent::Hubris {
            set_hubris_transient_override(None);
        }
        Ok(())
    }

    fn component_switch_default_image(
        &mut self,
        _: &userlib::RecvMessage,
        component: RotComponent,
        slot: SlotId,
        duration: SwitchDuration,
    ) -> Result<(), RequestError<UpdateError>> {
        match component {
            RotComponent::Hubris => {
                self.switch_default_hubris_image(slot, duration)
            }
            RotComponent::Stage0 => {
                match slot {
                    // Stage0
                    SlotId::A => {
                        Err(UpdateError::InvalidSlotIdForOperation.into())
                    }
                    // Stage0Next
                    SlotId::B => self.switch_default_boot_image(duration),
                }
            }
        }
    }
}

impl NotificationHandler for ServerImpl<'_> {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
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

    fn switch_default_boot_image(
        &mut self,
        duration: SwitchDuration,
    ) -> Result<(), RequestError<UpdateError>> {
        if duration != SwitchDuration::Forever {
            return Err(UpdateError::NotImplemented.into());
        }

        // Any image that passes the NXP image checks can be written to
        // stage0next flash slot.
        // The image checks ensure that the boot ROM will be able
        // to check the image signature without crashing.
        //
        // Only an image that has a valid signature seen at rot-startup
        // time will be copied by update-server to the stage0 partition.
        //
        // Note that both stage0 and stage0next images can be
        // modified multiple times between resets.
        // So, although we trust the bootstate(), we do not implicitly trust
        // the current contents of stage0 or stage0next unless the image
        // hash matches an image seen at rot-startup.
        //
        // The typical flow is that:
        //   1. A new bootloader image is staged by update-server.
        //   2. The RoT is reset.
        //   3. Signatures are evaluated in rot-startup.
        //   4. The validated staged image is copied by update-server
        //      to the bootloader slot.
        //   5. The RoT is rebooted into the new stage0 image.
        //
        // During step 4, there is a time where the RoT is not bootable.
        // Failures during step 4 may require a service call or RMA to fix
        // the system.
        //
        // Only one Gimlet, PSC, or SideCar in a rack should have their RoT
        // bootloader updated at a time to minimize the blast-radius of
        // failures such as a rack wide power issue.
        //
        // All of the stage0next slots in the rack can be updated (steps 1
        // through 3) to ensure that RoT image signatures are valid before any
        // system continues to step 4.
        //
        // TBD: While Failures up to step 3 do not adversly affect the RoT,
        // resetting the RoT to evaluate signatures may be service affecting
        // to the system depending on how the RoT and SP interact with respect
        // to their reset handling and the RoT measurement of the SP.
        // Appropriate measures should be taken to minimize customer impact.
        //
        // Note that after copying stage0next to stage0, but before step 5
        // (reset), it is still possible to revert to the old stage0 by
        // updating stage0next to the original stage0 contents that were
        // validated reset and then copying those to stage0.
        //
        // It is assumed that a hash collision is not computaionally feasible
        // for either the image hash done by rot-startup or used by the ROM
        // signature routine.

        // Read stage0next contents into RAM.
        let staged = image_range(RotComponent::Stage0, SlotId::B);
        let len = self.read_flash_image_to_cache(staged.0)?;
        let bootloader = &self.fw_cache[..len / core::mem::size_of::<u32>()];

        let mut hash = Sha3_256::new();
        for page in bootloader.as_bytes().chunks(512) {
            hash.update(page);
        }
        let cache_hash: [u8; 32] = hash.finalize().into();
        let boot_state =
            bootstate().map_err(|_| UpdateError::MissingHandoffData)?;

        // The cached image needs to match a properly signed image seen at
        // boot time.
        // Since stage0 and stage0next can be mutated after boot, we don't
        // compare to their current contents.
        // We are trusting that the recorded hash and signature status have
        // not been altered since boot and that hash collisions are not an issue.
        if !(boot_state.stage0next.status.is_ok()
            && cache_hash.as_bytes() == boot_state.stage0next.digest
            || boot_state.stage0.status.is_ok()
                && cache_hash.as_bytes() == boot_state.stage0.digest)
        {
            return Err(UpdateError::SignatureNotValidated.into());
        };

        // Don't risk an update if the cache already matches the bootloader.
        let stage0 = image_range(RotComponent::Stage0, SlotId::A);
        match self.compare_cache_to_flash(&stage0.0) {
            Err(UpdateError::ImageMismatch) => {
                if let Err(e) =
                    self.write_cache_to_flash(RotComponent::Stage0, SlotId::A)
                {
                    // N.B. An error here is bad since it means we've likely
                    // bricked the machine if we reset now.
                    // We do not want the RoT reset.
                    // Upper layers should try to write the image again.
                    // We don't have a valid copy of the code.
                    // TODO: Think about possible recovery if stage0
                    // has been corrupted.

                    // Restart update_server
                    return Err(e.into());
                }
            }
            Ok(()) => {
                // Unerased pages after an image are also hashed and
                // therefore contribute to the firmware ID.
                // This mechanism helps detect "dirty" flash banks
                // and the possible exfiltration of data or incomplete
                // update operations.
                // It will produce a false negative for image matching
                // but this is intended.
            }
            Err(e) => {
                return Err(e.into()); // Non-fatal error. We did not alter stage0.
            }
        }

        // Finish by erasing the unused portion of flash bank.
        // An error here means that the stage0 slot may not be clean but at least
        // it has the intended bootloader written.
        let erase_start = stage0.0.start.checked_add(len as u32).unwrap_lite();
        self.flash_erase_range(erase_start..stage0.0.end)?;
        Ok(())
    }

    fn flash_erase_range(
        &mut self,
        span: Range<u32>,
    ) -> Result<(), UpdateError> {
        // It's assumed that the caller has done safe math and that
        // there is no danger here.
        let word_span = (span.start / (BYTES_PER_FLASH_WORD as u32))
            ..=(span.end / (BYTES_PER_FLASH_WORD as u32) - 1);

        self.flash.start_erase_range(word_span);
        loop {
            match self.flash.poll_erase_or_program_result() {
                None => continue,
                Some(Ok(())) => return Ok(()),
                Some(Err(_)) => return Err(UpdateError::FlashError),
            }
        }
    }

    fn compare_cache_to_flash(
        &self,
        span: &Range<u32>,
    ) -> Result<(), UpdateError> {
        // Is there a cached image?
        // no, return error

        // Lengths are rounded up to a flash page boundary.
        let clen = self.cache_image_len()?;
        let flen = self.flash_image_len(span)?;
        if clen != flen {
            return Err(UpdateError::ImageMismatch);
        }
        // compare flash page to cache
        let cached =
            self.fw_cache[0..flen / core::mem::size_of::<u32>()].as_bytes();
        let mut flash_page = [0u8; BYTES_PER_FLASH_PAGE];
        for addr in (0..flen).step_by(BYTES_PER_FLASH_PAGE) {
            let size = if addr + BYTES_PER_FLASH_PAGE > flen {
                flen - addr
            } else {
                BYTES_PER_FLASH_PAGE
            };

            indirect_flash_read(
                &self.flash,
                addr as u32,
                &mut flash_page[..size],
            )?;
            if flash_page[0..size] != cached[addr..addr + size] {
                return Err(UpdateError::ImageMismatch);
            }
        }
        Ok(())
    }

    // Looking at a region of flash, determine if there is a possible NXP
    // image programmed. Return the length in bytes of the flash pages
    // comprising the image including padding to fill to a page boundary.
    fn flash_image_len(&self, span: &Range<u32>) -> Result<usize, UpdateError> {
        let buf = &mut [0u32; 1];
        indirect_flash_read(
            &self.flash,
            span.start + LENGTH_OFFSET as u32,
            buf[..].as_mut_bytes(),
        )?;
        if let Some(len) = round_up_to_flash_page(buf[0]) {
            // The minimum image size should be further constrained
            // but this is enough bytes for an NXP header and not
            // bigger than the flash slot.
            if len as usize <= span.len() && len >= HEADER_OFFSET {
                return Ok(len as usize);
            }
        }
        Err(UpdateError::BadLength)
    }

    fn cache_image_len(&self) -> Result<usize, UpdateError> {
        let len = round_up_to_flash_page(
            self.fw_cache[LENGTH_OFFSET / core::mem::size_of::<u32>()],
        )
        .ok_or(UpdateError::BadLength)?;

        if len as usize > self.fw_cache.as_bytes().len() || len < HEADER_OFFSET
        {
            return Err(UpdateError::BadLength);
        }
        Ok(len as usize)
    }

    fn read_flash_image_to_cache(
        &mut self,
        span: Range<u32>,
    ) -> Result<usize, UpdateError> {
        // Returns error if flash page is erased.
        let staged = image_range(RotComponent::Stage0, SlotId::B);
        let len = self.flash_image_len(&staged.0)?;
        if len as u32 > span.end || len > self.fw_cache.as_bytes().len() {
            return Err(UpdateError::BadLength);
        }
        indirect_flash_read(
            &self.flash,
            span.start,
            self.fw_cache[0..len / core::mem::size_of::<u32>()].as_mut_bytes(),
        )?;
        Ok(len)
    }

    fn write_cache_to_flash(
        &mut self,
        component: RotComponent,
        slot: SlotId,
    ) -> Result<(), UpdateError> {
        let clen = self.cache_image_len()?;
        if !clen.is_multiple_of(BYTES_PER_FLASH_PAGE) {
            return Err(UpdateError::BadLength);
        }
        let span = image_range(component, slot).0;
        if span.end < span.start + clen as u32 {
            return Err(UpdateError::BadLength);
        }
        // Sanity check could be repeated here.
        // erase/write each flash page.
        let chunks = self.fw_cache[..]
            .as_bytes()
            .chunks_exact(BYTES_PER_FLASH_PAGE);
        for (block_num, block) in chunks.enumerate() {
            let flash_page = block.try_into().unwrap_lite();
            do_block_write(
                &mut self.flash,
                component,
                slot,
                block_num,
                flash_page,
            )?;
        }
        Ok(())
    }

    fn switch_default_hubris_image(
        &mut self,
        slot: SlotId,
        duration: SwitchDuration,
    ) -> Result<(), RequestError<UpdateError>> {
        match duration {
            SwitchDuration::Once => {
                // TODO check Rollback policy vs epoch before activating.
                // TODO: prep-image-update should clear transient selection.
                //   e.g. update, activate, update, reboot should not have
                //   transient boot set.
                set_hubris_transient_override(Some(slot));
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
                #[allow(clippy::bool_to_int_with_if)]
                let new_bit = if slot == SlotId::A { 0 } else { 1 };
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
/// corresponding block of 16 bytes is using `zerocopy::IntoBytes` to reinterpret
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
            sys_recv_notification(notifications::FLASH_IRQ_MASK);
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

/// Performs an erase-write sequence to a single page within a given target
/// image.
fn do_block_write(
    flash: &mut drv_lpc55_flash::Flash<'_>,
    component: RotComponent,
    slot: SlotId,
    block_num: usize,
    flash_page: &[u8; BLOCK_SIZE_BYTES],
) -> Result<(), UpdateError> {
    // The update.idol definition uses usize; our hardware uses u32; convert
    // here so we don't have to cast everywhere.
    let page_num = block_num as u32;

    // Can only update opposite image
    if same_image(component, slot) {
        return Err(UpdateError::RunningImage);
    }

    let write_addr = match target_addr(component, slot, page_num) {
        Some(addr) => addr,
        None => return Err(UpdateError::OutOfBounds),
    };

    flash
        .write_page(write_addr, flash_page, wait_for_flash_interrupt)
        .map_err(|_| UpdateError::FlashError)
}

fn wait_for_flash_interrupt() {
    sys_irq_control(notifications::FLASH_IRQ_MASK, true);
    sys_recv_notification(notifications::FLASH_IRQ_MASK);
}

/// Computes the byte address of the first byte in a
/// particular (component, slot, page) combination.
///
/// `page_num` designates a flash page (called a block elsewhere in this file, a
/// 512B unit) within the flash slot. If the page is out range for the target
/// slot, returns `None`.
fn target_addr(
    component: RotComponent,
    slot: SlotId,
    page_num: u32,
) -> Option<u32> {
    let range = image_range(component, slot).0;

    // This is safely calculating addr = base + page_num * PAGE_SIZE
    let addr = page_num
        .checked_mul(BLOCK_SIZE_BYTES as u32)
        .and_then(|n| n.checked_add(range.start))?;

    // check addr + PAGE_SIZE <= end
    if addr.checked_add(BLOCK_SIZE_BYTES as u32)? > range.end {
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
    component: RotComponent,
    slot: SlotId,
) -> Result<core::ops::Range<u32>, RawCabooseError> {
    let flash_range = image_range(component, slot).0;

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
        flash_range.start + HEADER_OFFSET,
        header.as_mut_bytes(),
    )
    .map_err(|_| RawCabooseError::ReadFailed)?;
    if header.magic != HEADER_MAGIC {
        return Err(RawCabooseError::NoImageHeader);
    }

    // Calculate where the image header implies that the image should end
    //
    // This is a one-past-the-end value.
    let image_end = flash_range.start + header.total_image_len;

    // Then, check that value against the BANK2 bounds.
    //
    // Safety: populated by the linker, so this should be valid
    if image_end > flash_range.end {
        return Err(RawCabooseError::MissingCaboose);
    }

    // By construction, the last word of the caboose is its size as a `u32`
    let mut caboose_size = 0u32;
    indirect_flash_read(
        flash,
        image_end - U32_SIZE,
        caboose_size.as_mut_bytes(),
    )
    .map_err(|_| RawCabooseError::ReadFailed)?;

    let caboose_start = image_end.saturating_sub(caboose_size);
    let caboose_range = if caboose_start < flash_range.start {
        // This branch will be encountered if there's no caboose, because
        // then the nominal caboose size will be 0xFFFFFFFF, which will send
        // us out of the bank2 region.
        return Err(RawCabooseError::MissingCaboose);
    } else {
        // Safety: we know this pointer is within the programmed flash region,
        // since it's checked above.
        let mut v = 0u32;
        indirect_flash_read(flash, caboose_start, v.as_mut_bytes())
            .map_err(|_| RawCabooseError::ReadFailed)?;
        if v == CABOOSE_MAGIC {
            caboose_start + U32_SIZE..image_end - U32_SIZE
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
        return Err(RequestError::Fail(ClientError::BadLease));
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
        return Err(RequestError::Fail(ClientError::BadLease));
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

fn bootstate() -> Result<RotBootStateV2, HandoffDataLoadError> {
    // Safety: Data is published by stage0
    let addr = unsafe { BOOTSTATE.assume_init_ref() };
    RotBootStateV2::load_from_addr(addr)
}

fn set_transient_override(preference: [u8; 32]) {
    // Safety: Data is consumed by Bootleby on next boot.
    // There are no concurrent writers possible.
    // Calling this function multiple times is ok.
    // Bootleby is careful to vet contents before acting.
    unsafe {
        TRANSIENT_OVERRIDE.write(preference);
    }
}

pub fn set_hubris_transient_override(bank: Option<SlotId>) {
    // Preference constants are taken from bootleby:src/lib.rs
    const PREFER_SLOT_A: [u8; 32] = hex!(
        "edb23f2e9b399c3d57695262f29615910ed10c8d9b261bfc2076b8c16c84f66d"
    );
    const PREFER_SLOT_B: [u8; 32] = hex!(
        "70ed2914e6fdeeebbb02763b96da9faa0160b7fc887425f4d45547071d0ce4ba"
    );
    // Bootleby writes all zeros after reading. We write all ones to reset.
    const PREFER_NOTHING: [u8; 32] = [0xffu8; 32];

    match bank {
        // Do we need a  value that says we were here and cleared?
        None => set_transient_override(PREFER_NOTHING),
        Some(SlotId::A) => set_transient_override(PREFER_SLOT_A),
        Some(SlotId::B) => set_transient_override(PREFER_SLOT_B),
    }
}

fn round_up_to_flash_page(offset: u32) -> Option<u32> {
    offset.checked_next_multiple_of(BYTES_PER_FLASH_PAGE as u32)
}

task_slot!(SYSCON, syscon);
task_slot!(JEFE, jefe);

#[export_name = "main"]
fn main() -> ! {
    let syscon = drv_lpc55_syscon_api::Syscon::from(SYSCON.get_task_id());

    // Go ahead and put the HASHCRYPT unit into reset.
    syscon.enter_reset(drv_lpc55_syscon_api::Peripheral::HashAes);
    let fw_cache = mutable_statics::mutable_statics! {
        static mut FW_CACHE: [u32; FW_CACHE_MAX / core::mem::size_of::<u32>()] = [|| 0; _];
    };
    let mut server = ServerImpl {
        header_block: None,
        state: UpdateState::NoUpdate,
        image: None,

        flash: drv_lpc55_flash::Flash::new(unsafe {
            &*lpc55_pac::FLASH::ptr()
        }),
        hashcrypt: unsafe { &*lpc55_pac::HASHCRYPT::ptr() },
        syscon,
        fw_cache,
        next_block: None,
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
        RotComponent, RotPage, SlotId, SwitchDuration, UpdateTarget,
        VersionedRotBootInfo,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
