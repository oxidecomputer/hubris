// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the caboose reader task

#![no_std]
use derive_idol_err::IdolError;
use userlib::FromPrimitive;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
pub enum CabooseError {
    MissingCaboose = 1,
    TlvcReaderBeginFailed = 2,
    TlvcReadExactFailed = 3,
    NoSuchTag = 4,
}
