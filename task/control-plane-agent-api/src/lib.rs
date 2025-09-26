// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for the Control Plane Agent task.

#![no_std]

use derive_idol_err::IdolError;
use serde::{Deserialize, Serialize};
use userlib::*;

pub use host_sp_messages::HostStartupOptions;
pub use oxide_barcode::OxideIdentity;
pub use oxide_barcode::ParseError as BarcodeParseError;

/// Maximum length (in bytes) allowed for installinator image ID blobs.
pub const MAX_INSTALLINATOR_IMAGE_ID_LEN: usize = 512;

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum ControlPlaneAgentError {
    DataUnavailable = 1,
    InvalidStartupOptions,
    OperationUnsupported,
    MgsAttachedToUart,

    #[idol(server_death)]
    ServerRestarted,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, counters::Count,
)]
pub enum UartClient {
    Mgs,
    Humility,
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
