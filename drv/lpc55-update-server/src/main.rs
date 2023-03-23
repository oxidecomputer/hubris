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
use drv_update_api::{UpdateError, UpdateStatus, UpdateTarget};
use hypocalls::*;
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

}

#[used]
#[link_section = ".bootstate"]
static BOOTSTATE: MaybeUninit<[u8; 0x1000]> = MaybeUninit::uninit();

cfg_if::cfg_if! {
    if #[cfg(any(target_board = "lpcxpresso55s69", target_board = "gimlet-c"))]{
        declare_tz_table!();
    } else {
        declare_not_tz_table!();
    }
}

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

const BLOCK_SIZE_BYTES: usize = FLASH_PAGE_SIZE;

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

    let reset_vector = u32::from_le_bytes(
        block[RESET_VECTOR_OFFSET..][..4].try_into().unwrap_lite(),
    );
    let a_base = unsafe { &__IMAGE_A_BASE } as *const u32 as u32;
    let b_base = unsafe { &__IMAGE_B_BASE } as *const u32 as u32;
    let stage0_base = unsafe { &__IMAGE_STAGE0_BASE } as *const u32 as u32;
    let a_end = unsafe { &__IMAGE_A_END } as *const u32 as u32;
    let b_end = unsafe { &__IMAGE_B_END } as *const u32 as u32;
    let stage0_end = unsafe { &__IMAGE_STAGE0_END } as *const u32 as u32;

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

fn do_block_write(
    img: UpdateTarget,
    block_num: usize,
    flash_page: &mut [u8; BLOCK_SIZE_BYTES],
) -> Result<(), UpdateError> {
    let result = unsafe {
        // The write_to_flash API takes raw pointers due to TrustZone
        // ABI requirements which makes this function unsafe.
        tz_table!().write_to_flash(
            img,
            block_num as u32,
            flash_page.as_mut_ptr(),
        )
    };

    match result {
        HypoStatus::Success => Ok(()),
        HypoStatus::OutOfBounds => Err(UpdateError::OutOfBounds),
        HypoStatus::RunningImage => Err(UpdateError::RunningImage),
        // Should probably encode the LPC55 flash status into the update
        // error for good measure but that takes effort...
        HypoStatus::FlashError(_) => Err(UpdateError::FlashError),
    }
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
mod idl {
    use super::{
        CabooseError, ImageVersion, UpdateError, UpdateStatus, UpdateTarget,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
