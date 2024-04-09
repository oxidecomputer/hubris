// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for El Jefe

#![no_std]

use derive_idol_err::IdolError;
pub use dump_agent_api::DumpAgentError;
pub use humpty::DumpArea;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
#[repr(C)]
pub enum DumpAreaError {
    InvalidIndex = 1,
    AlreadyInUse,
}

#[macro_export]
macro_rules! impl_dump {
    (impl Dump for $Ty:ident {}) => {
        impl $crate::Dump for $Ty {
            fn reinitialize_dump_areas(
                &self,
            ) -> Result<(), $crate::DumpAgentError> {
                $Ty::reinitialize_dump_areas(self)
            }

            fn get_dump_area(
                &self,
                index: u8,
            ) -> Result<$crate::DumpArea, $crate::DumpAgentError> {
                $Ty::get_dump_area(self, index)
            }

            fn claim_dump_area(
                &self,
            ) -> Result<$crate::DumpArea, $crate::DumpAgentError> {
                $Ty::claim_dump_area(self)
            }

            fn dump_task(
                &self,
                task_index: u32,
            ) -> Result<u8, $crate::DumpAgentError> {
                $Ty::dump_task(self, task_index)
            }

            fn dump_task_region(
                &self,
                task_index: u32,
                address: u32,
                length: u32,
            ) -> Result<u8, $crate::DumpAgentError> {
                $Ty::dump_task_region(self, task_index, address, length)
            }

            fn reinitialize_dump_from(
                &self,
                index: u8,
            ) -> Result<(), $crate::DumpAgentError> {
                $Ty::reinitialize_dump_from(self, index)
            }
        }
    };
}

pub trait Dump {
    fn reinitialize_dump_areas(&self) -> Result<(), DumpAgentError>;

    fn get_dump_area(&self, index: u8) -> Result<DumpArea, DumpAgentError>;

    fn claim_dump_area(&self) -> Result<DumpArea, DumpAgentError>;

    fn dump_task(&self, task_index: u32) -> Result<u8, DumpAgentError>;

    fn dump_task_region(
        &self,
        task_index: u32,
        address: u32,
        length: u32,
    ) -> Result<u8, DumpAgentError>;

    fn reinitialize_dump_from(&self, index: u8) -> Result<(), DumpAgentError>;
}
