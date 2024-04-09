// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for El Jefe

#![no_std]
pub use dump_types::{DumpAgentError, DumpArea};
use userlib::*;

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));

dump_types::impl_dump! {
    impl Dump for Jeffrey {}
}
