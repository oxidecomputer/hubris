// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Items that are unique to SPs with a host, e.g. compute sleds.

/// Metadata about panics observed from the host
pub struct HostPanicMetadata {
    /// Length in bytes of the currently stored panic message
    pub total_length: usize,
    /// (hopefully not) Rolling counter of panic messages observed this power
    /// cycle
    pub sequence_number: u32,
    /// Boot slot
    pub slot: Option<u16>,
}

/// Metadata about panics observed from the host
pub struct HostBootFailMetadata {
    /// Length in bytes of the currently stored bootfail message
    pub total_length: usize,
    /// (hopefully not) Rolling counter of panic messages observed this power
    /// cycle
    pub sequence_number: u32,
    /// Bootfail reason
    pub reason: u8,
    /// Boot slot
    pub slot: Option<u16>,
}

/// Data we store from the host in case it crashes, either early as a BootFail,
/// or later as a panic.
pub struct HostCrashDebuggingInfo {
    pub panic_payload: [u8; 4096],
    pub bootfail_payload: [u8; 4096],
    pub panic_state: Option<HostPanicMetadata>,
    pub bootfail_state: Option<HostBootFailMetadata>,
}

impl HostCrashDebuggingInfo {
    pub const fn new() -> Self {
        Self {
            panic_payload: [0u8; _],
            bootfail_payload: [0u8; _],
            panic_state: None,
            bootfail_state: None,
        }
    }
}
