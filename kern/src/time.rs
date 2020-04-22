/// In-kernel timestamp representation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
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
