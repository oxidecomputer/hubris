// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Ereport message definitions shared between multiple tasks.

#![no_std]

pub mod cpu;
pub mod pwr;

#[cfg(feature = "ereporter-macro")]
#[macro_export]
macro_rules! declare_ereporter {
    ($($v:vis)? struct $Ereporter:ident<$Trait:ident> { $($EreportTy:ty),+ $(,)? }) => {
        $($v)? struct $Ereporter {

        }
    };
}

#[cfg(feature = "ereporter-macro")]
#[doc(hidden)]
pub mod __macro_support {
    pub use paste::paste;
}
