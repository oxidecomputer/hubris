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
    ($v:vis struct $Ereporter:ident<$Trait:ident> {
        $($ClassName:ident($EreportTy:ty)),+ $(,)?
    }) => {
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
                    [< $Ereporter:snake >]::deliver_ereport(self, ereport);
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
                            EreportClass::$ClassName
                        }
                    }
                )+

                $crate::__macro_support::ringbuf::counted_ringbuf!(
                    __EREPORT_RINGBUF,
                    EreportTrace,
                    8,
                    EreportTrace::None
                );

                #[derive($crate::__macro_support::counters::Count, Eq, PartialEq, Copy, Clone)]
                pub(super) enum EreportClass {
                    $(
                        $ClassName
                    ),+
                }

                #[derive($crate::__macro_support::counters::Count, Eq, PartialEq, Copy, Clone)]
                enum EreportTrace {
                    #[count(skip)]
                    None,
                    EreportSent {#[count(children)] class: EreportClass, len: usize },
                    EreportLost {
                        #[count(children)] class: EreportClass,
                        len: usize,
                        err:  task_packrat_api::EreportWriteError
                    },
                    EreportTooBig { #[count(children)] class: EreportClass },
                }

                pub(super) fn deliver_ereport(this: &mut $Ereporter, ereport: &impl $Trait) {
                    use $crate::__macro_support::ringbuf::ringbuf_entry;
                    let class = ereport.class();
                    let eresult = this
                        .packrat
                        .deliver_microcbor_ereport(&ereport, &mut this.buf[..]);
                    match eresult {
                        Ok(len) => {
                            ringbuf_entry!(
                                __EREPORT_RINGBUF,
                                EreportTrace::EreportSent{ len, class }
                            );
                        }
                        Err(task_packrat_api::EreportEncodeError::Packrat { len, err }) => {
                            ringbuf_entry!(
                                __EREPORT_RINGBUF,
                                EreportTrace::EreportLost { len, class, err }
                            );
                        }
                        Err(task_packrat_api::EreportEncodeError::Encoder(_)) => {
                            ringbuf_entry!(
                                __EREPORT_RINGBUF,
                                EreportTrace::EreportTooBig { class }
                            );
                        }
                    }

                }

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
    pub use counters;
    pub use microcbor::StaticCborLen;
    pub use microcbor::max_cbor_len_for;
    pub use paste::paste;
    pub use ringbuf;
    pub use static_cell::ClaimOnceCell;
}
