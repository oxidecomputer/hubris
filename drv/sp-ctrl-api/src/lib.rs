// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the SPI server

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
#[repr(u32)]
pub enum SpCtrlError {
    BadLen = 1,
    NeedInit,
    Fault,
    InvalidCoreRegister,
    DongleDetected,

    #[idol(server_death)]
    ServerRestarted,
}

impl SpCtrl {
    pub fn write_word_32(
        &self,
        addr: u32,
        val: u32,
    ) -> Result<(), SpCtrlError> {
        self.write(addr, &val.to_le_bytes())
    }

    pub fn read_word_32(&self, addr: u32) -> Result<u32, SpCtrlError> {
        let mut bytes: [u8; 4] = [0; 4];

        match self.read(addr, &mut bytes) {
            Err(e) => return Err(e),
            _ => (),
        }

        Ok(u32::from_le_bytes(bytes))
    }
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
