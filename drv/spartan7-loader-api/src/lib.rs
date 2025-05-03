// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Spartan7 loader.

#![no_std]

use userlib::sys_send;

/// Token that indicates that the Spartan-7 is running
///
/// This token can be passed to peripheral constructors.
pub struct Spartan7Token(());

impl Spartan7Loader {
    /// Gets a token proving that the Spartan-7 is running
    pub fn get_token(&self) -> Spartan7Token {
        self.ping();
        Spartan7Token(())
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
