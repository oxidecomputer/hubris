//! Implementation of kernel time.

use ufmt::derive::uDebug;

/// In-kernel timestamp representation.
///
/// This is currently measured in an arbitrary "tick" unit.
#[derive(Copy, Clone, uDebug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(transparent)]
pub struct Timestamp(u64);

impl From<u64> for Timestamp {
    fn from(v: u64) -> Self {
        Timestamp(v)
    }
}

impl From<Timestamp> for u64 {
    fn from(v: Timestamp) -> Self {
        v.0
    }
}
