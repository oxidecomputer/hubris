// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the SPI server

#![no_std]

use derive_idol_err::IdolError;
use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError)]
#[repr(u32)]
#[derive(counters::Count)]
pub enum SpCtrlError {
    BadLen = 1,
    NeedInit,
    Fault,
    InvalidCoreRegister,
    DongleDetected,

    #[idol(server_death)]
    ServerRestarted,
    Timeout,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
