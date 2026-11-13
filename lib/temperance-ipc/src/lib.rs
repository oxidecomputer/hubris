// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `temperance-ipc`
//!
//! This is a helper crate for using [`temperance`] with Hubris IPC.
//!
//! This uses the standard `temperance` encoding form, including the 12-byte
//! frame header for requests and responses.
//!
//! This library supports both "inline" requests/responses, where the
//! 256-byte-max input/output buffers are used, or optionally allows
//! requests/responses to be transferred using leases. Support for leases is
//! opt-in, and requires the server and client to both use the "leases" feature.
//!
//! For clients/servers that never work with requests or responses larger than
//! 256, leases can be avoided entirely to reduce additional system calls by
//! the server to read/write leases.
//!
//! ## `operation` codes
//!
//! `temperance-ipc` uses the `operation` field of the `SYS_SEND` call to encode
//! where the message frame is stored. A base value of `0x7E00` is used for all
//! opcodes.
//!
//! When the "leases" feature is not selected, inline buffers are always used.
//! Requests/responses exceeding 256 bytes will always fail.
//!
//! When the "leases" feature is selected, the client will automatically use
//! inline buffers when the request/response buffers are <= 256 bytes, and
//! automatically switch to leases only when necessary.
//!
//! This allows for a server that takes a short request and long response (e.g.
//! a "get bulk" request) to use an inline request and leased response.
//! Similarly, it allows for a server that takes a long request and short
//! response (e.g. a "send bulk" request) to use a leased request and inline
//! response.
//!
//! From the client's buffer naming perspective:
//!
//! | Opcode   | Request in      | Response in      | Needs "leases" |
//! |----------|-----------------|------------------|----------------|
//! | `0x7E00` | `outgoing`      | `incoming`       | no             |
//! | `0x7E01` | lease 0 (read)  | `incoming`       | yes            |
//! | `0x7E02` | `outgoing`      | lease 0 (write)  | yes            |
//! | `0x7E03` | lease 0 (read)  | lease 1 (write)  | yes            |
//!
//! When the request is inline, the request and length are stored using the
//! `outgoing` buffer contents and length.
//!
//! When the request is in a lease, the `outgoing` buffer is unused (zero
//! length), and the contents and length are stored in the lease.
//!
//! When the response is inline, the response is stored in the `incoming`
//! buffer, and the length used is provided as part of the SYS_SEND return
//! values.
//!
//! When the response is in a lease, the `incoming` buffer is used to return
//! the length of the response as a `u32`, and a value of 4 is provided as part
//! of the SYS_SEND return values.

#![no_std]

pub mod client;
pub mod server;

pub use client::{IpcClient, IpcClientError, IpcClientIo};
pub use server::{IpcBackend, IpcMeta, IpcServer, IpcServerError};

/// Base value of opcodes, used to avoid confusion with other formats.
pub const OP_BASE: u16 = 0x7E00;

/// Mask used for opcode settings
pub const OP_FLAGS_MASK: u16 = 0x00FF;

/// The request is stored in a lease.
pub const FLAG_RQST_LEASED: u16 = 1 << 0;

/// The response is stored in a lease.
pub const FLAG_RESP_LEASED: u16 = 1 << 1;

/// Are leases allowed at all?
///
/// Stored as a constant value to help the optimizer remove dead code paths.
pub const LEASES: bool = cfg!(feature = "leases");

/// What flags are allowed?
///
/// Stored as a constant value to help the optimizer remove dead code paths.
pub const SUPPORTED_FLAGS: u16 = if LEASES {
    FLAG_RQST_LEASED | FLAG_RESP_LEASED
} else {
    0
};

/// Length that will trigger a switch to using leases. Must be <= the maximum
/// inline length for `SYS_SEND`.
pub const LEASE_THRESHOLD: usize = 256;

/// Length written to the client's `incoming` buffer when the response is stored
/// in a lease.
pub const RESP_LEN_SIZE: usize = core::mem::size_of::<u32>();

/// Success response code.
///
/// When this response code is returned, the response is of the type expected
/// by the client.
pub const REPLY_OK: u32 = 0;

/// Failure response code.
///
/// When this response code is returned, the response is of the WireError type.
pub const REPLY_SERVER_FAILED: u32 = 1;

/// Encode the opcode with the given flags
pub const fn operation(flags: u16) -> u16 {
    OP_BASE | (flags & OP_FLAGS_MASK)
}

/// Decode the flags set in the opcode field
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
#[doc(hidden)]
pub mod __private {
    pub use temperance;
}

/// TODO: Add docs
#[macro_export]
macro_rules! expose_interface {
    ($($segment:ident)::+) => {
        // TODO(robo): Mach-O needs a "segment,section" name; skipped on Apple
        // hosts for now.
        #[cfg(not(target_vendor = "apple"))]
        const _: () = {
            $crate::__private::temperance::flat_interface! {
                #[used]
                #[unsafe(link_section = ".temperance_ipc_schema")]
                static TEMPERANCE_IPC_SCHEMA = $($segment)::+;
            }
        };
    };
}
