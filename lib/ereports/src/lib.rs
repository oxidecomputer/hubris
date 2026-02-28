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

        $crate::__macro_support::paste! {

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
                    use [< $Ereporter:snake >]::*;
                    use $crate::__macro_support::ringbuf::ringbuf_entry;
                    let class = ereport.class();
                    let eresult = self
                        .packrat
                        .deliver_microcbor_ereport(&ereport, &mut self.buf[..]);
                    match eresult {
                        Ok(len) => {
                            ringbuf_entry!(Trace::EreportSent{ len, class });
                        }
                        Err(task_packrat_api::EreportEncodeError::Packrat { len, err }) => {
                            ringbuf_entry!(Trace::EreportLost(len, class, err))
                        }
                        Err(task_packrat_api::EreportEncodeError::Encoder(_)) => {
                            ringbuf_entry!(Trace::EreportTooBig { class });
                        }
                    }
                }
            }

            $v use [< $Ereporter:snake >]::$Trait;
            $v mod [< $Ereporter:snake >] {
                use super::*;
                use $crate::declare_ereporter;

                $v trait $Trait: $crate::__macro_support::StaticCborLen + sealed::Sealed {
                    fn class(&self) -> EreportClass;
                }

                $(
                    impl sealed::Sealed for $EreportTy {}
                    impl $Trait for $EreportTy {
                        fn class(&self) -> EreportClass {
                            declare_ereporter!(@ty_to_ident [] $EreportTy)
                        }
                    }
                )+

                ringbuf!();

                #[derive($crate::__macro_support::counters::Count, Eq, PartialEq, Copy, Clone)]
                pub(super) enum EreportClass {
                    $(
                      declare_ereporter!(@ty_to_ident [] $EreportTy)
                    ),+
                }

                #[derive($crate::__macro_support::counters::Count, Eq, PartialEq, Copy, Clone)]
                pub(super) enum Trace {
                    #[count(skip)]
                    None,
                    EreportSent {#[count(children)] class: EreportClass, len: usize },
                    EreportLost {
                        #[count(children)] class: EreportClass,
                        len: usize,
                        err:  packrat_api::EreportWriteError
                    },
                    EreportTooBig { #[count(children)] class: EreportClass },
                }

                mod sealed {
                    pub trait Sealed {}
                }
            }

        }
    };

    // -- Helper arms for converting types to enum variant names (internal use only) --

    // Base case: we've collected all identifiers, now concatenate them into a type name
    (@ty_to_ident $($e:ident)? [$first:ident $($rest:ident)*]) => {
        $crate::__macro_support::paste! {
            $($e::)?[< $first:cammel $($rest:camel)* >]
        }
    };
    // Match an identifier - add it to the accumulator
    (@ty_to_ident [$($acc:ident)*] $ident:ident $($rest:tt)*) => {
        declare_ereporter!(@ty_to_ident [$($acc)* $ident] $($rest)*)
    };
    // Skip any other token (::, <, >, {, }, commas, literals, etc.)
    (@ty_to_ident [$($acc:ident)*] $_:tt $($rest:tt)*) => {
        declare_ereporter!(@ty_to_ident [$($acc)*] $($rest)*)
    };
}

#[cfg(feature = "ereporter-macro")]
#[doc(hidden)]
pub mod __macro_support {
    pub use counters;
    pub use microcbor::StaticCborLen;
    pub use microcbor::max_cbor_len_for;
    pub use paste::paste;
    pub use ringbuf;
    pub use static_cell::ClaimOnceCell;
}
