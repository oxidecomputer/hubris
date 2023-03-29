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
use drv_caboose::CabooseError;
use drv_lpc55_flash::{BYTES_PER_FLASH_PAGE, BYTES_PER_FLASH_WORD};
use drv_update_api::{UpdateError, UpdateStatus, UpdateTarget};
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R};
use stage0_handoff::{HandoffData, ImageVersion, RotBootState};
use userlib::*;

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

enum UpdateState {
    NoUpdate,
    InProgress,
    Finished,
}

struct ServerImpl {
    header_block: Option<[u8; BLOCK_SIZE_BYTES]>,
    state: UpdateState,
    image: Option<UpdateTarget>,
}

// TODO: This is the size of the vector table on the LPC55. We should
// probably  get it from somewhere else directly.
const MAGIC_OFFSET: usize = 0x130;
const RESET_VECTOR_OFFSET: usize = 4;

const BLOCK_SIZE_BYTES: usize = BYTES_PER_FLASH_PAGE;

const MAX_LEASE: usize = 1024;
const HEADER_BLOCK: usize = 0;

impl idl::InOrderUpdateImpl for ServerImpl {
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
            UpdateState::NoUpdate => {
                return Err(UpdateError::UpdateNotStarted.into())
            }
            UpdateState::Finished => {
                return Err(UpdateError::UpdateAlreadyFinished.into())
            }
            UpdateState::InProgress => (),
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
        // a nice way to define the least length based on a constant.
        if len > BLOCK_SIZE_BYTES {
            return Err(UpdateError::BadLength.into());
        }

        let mut flash_page = [0u8; BLOCK_SIZE_BYTES];
        let target = self.image.unwrap_lite();

        if block_num == HEADER_BLOCK {
            let header_block =
                self.header_block.get_or_insert([0u8; BLOCK_SIZE_BYTES]);
            block
                .read_range(0..len as usize, &mut header_block[..])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
            header_block[len..].fill(0);
            if let Err(e) = validate_header_block(target, &header_block) {
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
                .read_range(0..len as usize, &mut flash_page)
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

            flash_page[len..].fill(0);
        }

        do_block_write(target, block_num, &mut flash_page)?;

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
            self.image.unwrap_lite(),
            HEADER_BLOCK,
            self.header_block.as_mut().unwrap_lite(),
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
    ) -> Result<UpdateStatus, RequestError<Infallible>> {
        // Safety: Data is published by stage0
        let addr = unsafe { BOOTSTATE.assume_init_ref() };
        let status = match RotBootState::load_from_addr(addr) {
            Ok(details) => UpdateStatus::Rot(details),
            Err(e) => UpdateStatus::LoadError(e),
        };
        Ok(status)
    }

    fn read_image_caboose(
        &mut self,
        _: &RecvMessage,
        _name: [u8; 4],
        _data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<u32, RequestError<CabooseError>> {
        Err(CabooseError::MissingCaboose.into()) // TODO
    }
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
        _ => false,
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
    img: UpdateTarget,
    block_num: usize,
    flash_page: &mut [u8; BLOCK_SIZE_BYTES],
) -> Result<(), UpdateError> {
    // The update.idol definition uses usize; our hardware uses u32; convert
    // here so we don't have to cast everywhere.
    let page_num = block_num as u32;

    let flash = unsafe { &*lpc55_pac::FLASH::ptr() };
    let mut flash = drv_lpc55_flash::Flash::new(flash);

    // Can only update opposite image
    if same_image(img) {
        return Err(UpdateError::RunningImage);
    }

    let write_addr = match target_addr(img, page_num) {
        Some(addr) => addr,
        None => return Err(UpdateError::OutOfBounds),
    };

    // Step one: erase the page.

    // write_addr is a byte address; page_num is a page index; but the actual
    // erase machinery operates in terms of flash words. A flash word is 16
    // bytes in length. So, we need to convert.
    //
    // Note that the range used here is _inclusive._
    //
    // We wind up needing this in u32 form a lot, so let's cast once and reuse.
    const WORDSZ: u32 = BYTES_PER_FLASH_WORD as u32;
    static_assertions::const_assert_eq!(
        BLOCK_SIZE_BYTES % BYTES_PER_FLASH_WORD,
        0
    );

    flash.start_erase_range(
        write_addr / WORDSZ
            ..=(write_addr + BLOCK_SIZE_BYTES as u32 - 1) / WORDSZ,
    );
    wait_for_erase_or_program(&mut flash)?;

    // Transfer each 16-byte flash word (page row) into the write registers in
    // the flash controller.
    for (i, row) in flash_page.chunks_exact(BYTES_PER_FLASH_WORD).enumerate() {
        // TODO: this will be unnecessary if array_chunks stabilizes
        let row: &[u8; BYTES_PER_FLASH_WORD] = row.try_into().unwrap_lite();

        flash.start_write_row(i as u32, row);
        while !flash.poll_write_result() {
            // spin - supposed to be very quick in hardware.
        }
    }

    // Now, program the whole page into non-volatile storage. This again uses
    // page indices, requiring a divide-by-16.
    flash.start_program(write_addr / WORDSZ);
    wait_for_erase_or_program(&mut flash)?;

    Ok(())
}

/// Utility function that does an interrupt-driven poll and sleep while the
/// flash controller finishes a write or erase.
fn wait_for_erase_or_program(
    flash: &mut drv_lpc55_flash::Flash,
) -> Result<(), UpdateError> {
    loop {
        if let Some(result) = flash.poll_erase_or_program_result() {
            return result.map_err(|_| UpdateError::FlashError);
        }

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

fn same_image(which: UpdateTarget) -> bool {
    get_base(which) == unsafe { __this_image.as_ptr() } as u32
}

/// Returns the byte address of the first byte of the given flash target slot,
/// or panics if you're holding it wrong.
fn get_base(which: UpdateTarget) -> u32 {
    (match which {
        UpdateTarget::ImageA => unsafe { __IMAGE_A_BASE.as_ptr() },
        UpdateTarget::ImageB => unsafe { __IMAGE_B_BASE.as_ptr() },
        UpdateTarget::Bootloader => unsafe { __IMAGE_STAGE0_BASE.as_ptr() },
        _ => unreachable!(),
    }) as u32
}

fn get_end(which: UpdateTarget) -> u32 {
    (match which {
        UpdateTarget::ImageA => unsafe { __IMAGE_A_END.as_ptr() },
        UpdateTarget::ImageB => unsafe { __IMAGE_B_END.as_ptr() },
        UpdateTarget::Bootloader => unsafe { __IMAGE_STAGE0_END.as_ptr() },
        _ => unreachable!(),
    }) as u32
}

/// Computes the byte address of the first byte in a particular (slot, page)
/// combination.
///
/// `image_target` designates the flash slot and must be `ImageA`, `ImageB`, or
/// `Bootloader`, despite containing many other variants. All other choices will
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

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl {
        header_block: None,
        state: UpdateState::NoUpdate,
        image: None,
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
        CabooseError, ImageVersion, UpdateError, UpdateStatus, UpdateTarget,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
