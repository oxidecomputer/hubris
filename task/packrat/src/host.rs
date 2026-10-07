// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Items that are unique to SPs with a host, e.g. compute sleds.

use static_cell::ClaimOnceCell;

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
///
/// We keep panic/bootfail payloads as separate `ClaimOnceCell` contents to
/// prevent the initializer for this function from taking up a lot of `.text`
/// space.
pub struct HostCrashDebuggingInfo {
    pub panic_payload: &'static mut [u8; PAYLOAD_SIZE],
    pub bootfail_payload: &'static mut [u8; PAYLOAD_SIZE],
    pub panic_state: Option<HostPanicMetadata>,
    pub bootfail_state: Option<HostBootFailMetadata>,
}

/// Number of bytes we retain of each kind of payload from the host.
const PAYLOAD_SIZE: usize = 4096;

impl HostCrashDebuggingInfo {
    /// This may only be called once.
    #[allow(dead_code)] // Not all BSPs have a host!
    pub fn new() -> Self {
        static PAYLOADS: ClaimOnceCell<[[u8; PAYLOAD_SIZE]; 2]> =
            ClaimOnceCell::new([[0u8; PAYLOAD_SIZE]; 2]);
        let [panic_payload, bootfail_payload] = PAYLOADS.claim();
        Self {
            panic_payload,
            bootfail_payload,
            panic_state: None,
            bootfail_state: None,
        }
    }
}
