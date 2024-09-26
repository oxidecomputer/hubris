// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::data_region::DataRegion;
use hubpack::SerializedSize;
use serde::Deserialize;
use stage0_handoff::{HandoffData, HandoffDataLoadError};

pub enum HandoffDataRegion {
    DiceCerts,
    DiceRng,
}

pub const DICE_CERTS: HandoffDataRegion = HandoffDataRegion::DiceCerts;
pub const DICE_RNG: HandoffDataRegion = HandoffDataRegion::DiceRng;

// This file is generated by the crate build.rs.
mod build {
    include!(concat!(env!("OUT_DIR"), "/rng-config.rs"));
}

use build::{DICE_CERTS_REGION, DICE_RNG_REGION};

impl HandoffDataRegion {
    pub fn data_region(&self) -> DataRegion {
        match self {
            Self::DiceCerts => DICE_CERTS_REGION,
            Self::DiceRng => DICE_RNG_REGION,
        }
    }

    /// Load a type implementing HandoffData (and others) from a config::DataRegion.
    /// Errors will be reported in the ringbuf and will return None.
    #[inline(always)]
    pub fn load_data<
        T: for<'a> Deserialize<'a> + HandoffData + SerializedSize,
    >(
        &self,
    ) -> Result<T, HandoffDataLoadError> {
        use core::slice;

        let region = self.data_region();
        // Safety: This memory is setup by code executed before hubris and
        // exposed using the kernel `extern-regions` mechanism. The safety of
        // this code is an extension of our trust in the hubris pre-main, kernel,
        // and build process.
        let data = unsafe {
            slice::from_raw_parts(region.address as *mut u8, region.size)
        };

        T::load_from_addr(data)
    }
}
