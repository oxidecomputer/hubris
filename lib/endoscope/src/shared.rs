// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::cell::UnsafeCell;
use core::marker::Sync;
pub use endoscope_abi::{Shared, State};

extern "C" {
    static FLASH_BASE: [u8; 0];
    static FLASH_SIZE: [u32; 0];
}

#[repr(C)]
pub struct SharedWrapper {
    shared: UnsafeCell<Shared>,
}

unsafe impl Sync for SharedWrapper {}

impl SharedWrapper {
    pub const fn new() -> Self {
        SharedWrapper {
            shared: UnsafeCell::new(Shared {
                magic: Shared::MAGIC,
                state: State::Preboot as u32,
                start: 0,
                len: 0,
                digest: [0xff_u8; 32],
            }),
        }
    }

    pub fn set_state(&self, state: State) {
        unsafe {
            (*self.shared.get()).state = state as u32;
        }
    }

    pub fn get_start(&self) -> *const u8 {
        unsafe { (*self.shared.get()).start as *const u8 }
    }

    pub fn get_len(&self) -> usize {
        unsafe { (*self.shared.get()).len as usize }
    }

    pub fn set_digest(&self, digest: &[u8; 32]) {
        unsafe {
            (*self.shared.get()).digest.copy_from_slice(digest);
        }
    }

    // Get around complaints about static initializations from statics.
    pub fn set_flash_area(&self) {
        unsafe {
            (*self.shared.get()).start = FLASH_BASE.as_ptr() as u32;
            (*self.shared.get()).len = FLASH_SIZE.as_ptr() as u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_wrapper() {
        let shared_wrapper = SharedWrapper::new();
        shared_wrapper.set_state(42);
        let digest = shared_wrapper.get_digest_mut();
        digest[0] = 1;

        unsafe {
            assert_eq!((*shared_wrapper.shared.get()).state, 42);
            assert_eq!((*shared_wrapper.shared.get()).digest[0], 1);
        }
    }
}

// Mark as used so that symbol remains in symbol table
#[used]
#[no_mangle]
pub static SHARED: SharedWrapper = SharedWrapper::new();
