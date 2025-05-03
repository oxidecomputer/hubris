// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::cell::UnsafeCell;
use core::marker::Sync;
pub use endoscope_abi::{Shared, State, DIGEST_SIZE};

#[repr(C)]
pub struct SharedWrapper {
    shared: UnsafeCell<Shared>,
}

unsafe impl Sync for SharedWrapper {}

impl SharedWrapper {
    pub const fn new() -> Self {
        SharedWrapper {
            shared: UnsafeCell::new(Shared {
                state: State::Preboot as u32,
                digest: [0xff_u8; DIGEST_SIZE],
            }),
        }
    }

    pub fn set_state(&self, state: State) {
        unsafe {
            (*self.shared.get()).state = state as u32;
        }
    }

    pub fn set_digest(&self, digest: &[u8; DIGEST_SIZE]) {
        unsafe {
            (*self.shared.get()).digest.copy_from_slice(digest);
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
#[no_mangle]
pub static SHARED: SharedWrapper = SharedWrapper::new();
