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
    ($v:vis struct $Ereporter:ident<$Trait:ident> { $($EreportTy:ty),+ $(,)? }) => {
        $v struct $Ereporter {
            packrat: task_packrat_api::Packrat,
            buf: &'static mut [u8; Self::BUF_LEN],
        }

        impl $Ereporter {
            const BUF_LEN: usize = $crate::__macro_support::max_cbor_len_for![
                $($EreportTy),+
            ];

            $v fn claim_static_resources(packrat: task_packrat_api::Packrat) -> Self {
                use $crate::__macro_support::ClaimOnceCell;
                static EREPORT_BUF: ClaimOnceCell<[u8; $Ereporter::BUF_LEN]> =
                    ClaimOnceCell::new([0; $Ereporter::BUF_LEN]);
                Self {
                    packrat,
                    buf: EREPORT_BUF.claim()
                }
            }

            $v fn deliver_ereport(&mut self, ereport: &impl $Trait) {
                let eresult = self
                    .packrat
                    .deliver_microcbor_ereport(&ereport, &mut self.buf[..]);
                match eresult {
                    Ok(len) => {
                        // TODO(eliza): add ringbuf
                        // ringbuf_entry!(Trace::EreportSent(len));
                    }
                    Err(task_packrat_api::EreportEncodeError::Packrat { len, err }) => {
                        // ringbuf_entry!(Trace::EreportLost(len, err))
                        let _ = len;
                        let _ = err;
                    }
                    Err(task_packrat_api::EreportEncodeError::Encoder(_)) => {
                        // ringbuf_entry!(Trace::EreportTooBig)
                    }
                }
            }
        }
        $crate::__macro_support::paste! {
            $v use [< $Ereporter:snake >]::$Trait;
            $v mod [< $Ereporter:snake >] {
                use super::*;
                $v trait $Trait: $crate::__macro_support::StaticCborLen + sealed::Sealed {}

                $(
                    impl sealed::Sealed for $EreportTy {}
                    impl $Trait for $EreportTy {}
                )+

                mod sealed {
                    pub trait Sealed {}
                }
            }

        }
    };
}

#[cfg(feature = "ereporter-macro")]
#[doc(hidden)]
pub mod __macro_support {
    pub use microcbor::StaticCborLen;
    pub use microcbor::max_cbor_len_for;
    pub use paste::paste;
    pub use static_cell::ClaimOnceCell;
}
