// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

#![no_std]

pub mod client;
pub mod server;

pub use client::{IpcClient, IpcClientError, IpcClientIo};

use postcard_schema_ng::Schema;
pub use server::{IpcBackend, IpcMeta, IpcServer, IpcServerError};
use temperance::{
    interface::InterfaceInfo,
    wire::{HEADER_LEN, WireError},
};

/// TODO: Add docs
// TODO(robo): 0x7E00 is arbitrary; pick a real value.
pub const OP_BASE: u16 = 0x7E00;

/// TODO: Add docs
pub const OP_FLAGS_MASK: u16 = 0x00FF;

/// TODO: Add docs
pub const FLAG_RQST_LEASED: u16 = 1 << 0;

/// TODO: Add docs
pub const FLAG_RESP_LEASED: u16 = 1 << 1;

/// TODO: Add docs
pub const LEASES: bool = cfg!(feature = "leases");

/// TODO: Add docs
pub const SUPPORTED_FLAGS: u16 = if LEASES {
    FLAG_RQST_LEASED | FLAG_RESP_LEASED
} else {
    0
};

/// TODO: Add docs
pub const LEASE_THRESHOLD: usize = 256;

/// TODO: Add docs
pub const RESP_LEN_SIZE: usize = core::mem::size_of::<u32>();

/// TODO: Add docs
pub const REPLY_OK: u32 = 0;

/// TODO: Add docs
pub const REPLY_SERVER_FAILED: u32 = 1;

/// TODO: Add docs
pub const fn operation(flags: u16) -> u16 {
    OP_BASE | (flags & OP_FLAGS_MASK)
}

/// TODO: Add docs
pub const fn decode_operation(op: u32) -> Option<u16> {
    if op & !(OP_FLAGS_MASK as u32) != OP_BASE as u32 {
        return None;
    }
    let flags = op as u16 & OP_FLAGS_MASK;
    if flags & !SUPPORTED_FLAGS != 0 {
        return None;
    }
    Some(flags)
}

/// TODO: Add docs
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RespLimit {
    /// TODO: Add docs
    pub key: u64,
    /// TODO: Add docs
    pub max_frame_len: Option<usize>,
}

/// TODO: Add docs
pub const fn response_limits<const N: usize>(
    info: &InterfaceInfo,
) -> [RespLimit; N] {
    assert!(info.methods.len() == N);
    let err = match WireError::SCHEMA.max_size() {
        Some(n) => n,
        None => panic!("WireError must have a max size"),
    };
    let mut out = [RespLimit {
        key: 0,
        max_frame_len: None,
    }; N];
    let mut i = 0;
    while i < N {
        let m = &info.methods[i];
        out[i] = RespLimit {
            key: u64::from_le_bytes(m.key.to_bytes()),
            max_frame_len: match m.resp_schema.max_size() {
                Some(n) if n > err => Some(HEADER_LEN + n),
                Some(_) => Some(HEADER_LEN + err),
                None => None,
            },
        };
        i += 1;
    }
    out
}

/// TODO: Add docs
#[macro_export]
macro_rules! response_limits {
    ($($segment:ident)::+) => {{
        const N: usize = $($segment)::+::info::INTERFACE_INFO.methods.len();
        static LIMITS: [$crate::RespLimit; N] = $crate::response_limits::<N>(
            &$($segment)::+::info::INTERFACE_INFO,
        );
        &LIMITS
    }};
}
