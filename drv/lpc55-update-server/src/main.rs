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
use drv_update_api::{UpdateError, UpdateStatus, UpdateTarget};
use hypocalls::*;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R};
use stage0_handoff::{HandoffData, ImageVersion, RotBootState};
use userlib::*;

#[used]
#[link_section = ".bootstate"]
static BOOTSTATE: MaybeUninit<[u8; 0x4000]> = MaybeUninit::uninit();

cfg_if::cfg_if! {
    if #[cfg(target_board = "lpcxpresso55s69")] {
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
    state: UpdateState,
    image: Option<UpdateTarget>,
}

const BLOCK_SIZE_BYTES: usize = FLASH_PAGE_SIZE;

const MAX_LEASE: usize = 1024;

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

        let mut flash_page: [u8; BLOCK_SIZE_BYTES] = [0; BLOCK_SIZE_BYTES];

        block
            .read_range(0..len as usize, &mut flash_page)
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        flash_page[len..].fill(0);

        let img = self.image.unwrap_lite();

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
            HypoStatus::OutOfBounds => Err(UpdateError::OutOfBounds.into()),
            HypoStatus::RunningImage => Err(UpdateError::RunningImage.into()),
            // Should probably encode the LPC55 flash status into the update
            // error for good measure but that takes effort...
            HypoStatus::FlashError(_) => Err(UpdateError::FlashError.into()),
        }
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
        let addr: &[u8] = unsafe { BOOTSTATE.assume_init_ref() };
        let status = match RotBootState::load_from_addr(&addr[0x2000..]) {
            Ok(details) => UpdateStatus::Rot(details),
            Err(e) => UpdateStatus::LoadError(e),
        };
        Ok(status)
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl {
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
    use super::{ImageVersion, UpdateError, UpdateStatus, UpdateTarget};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
