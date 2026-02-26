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
            packrat: task_packrat_api::Packrat,
            buf: &'static mut [u8; Self::BUF_LEN],
        }

        impl $Ereporter {
            const BUF_LEN: usize = $crate::__macro_support::max_cbor_len_for!($($EreportTy),+);
        }

        $($v)? trait $Trait: $crate::__macro_support::StaticCborLen {}

        $(
            impl $Trait for $EreportTy {}
        )+
    };
}

#[cfg(feature = "ereporter-macro")]
#[doc(hidden)]
pub mod __macro_support {
    pub use microcbor::max_cbor_len_for;
    pub use paste::paste;
}
