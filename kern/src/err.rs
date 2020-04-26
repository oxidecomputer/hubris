//! Common error-handling support.
//!
//! This module is designed around the idea that kernel code spends too much
//! time handling and recording errors, and we ought to be able to separate that
//! concern using `Result`.

use crate::task::{FaultInfo, Task, UsageError};

#[derive(Copy, Clone, Debug)]
pub enum UserError {
    Recoverable(u32),
    Unrecoverable(FaultInfo),
}

impl From<FaultInfo> for UserError {
    fn from(f: FaultInfo) -> Self {
        Self::Unrecoverable(f)
    }
}

impl From<UsageError> for UserError {
    fn from(f: UsageError) -> Self {
        Self::Unrecoverable(f.into())
    }
}

#[derive(Copy, Clone, Debug)]
pub struct InteractFault {
    pub src: Option<FaultInfo>,
    pub dst: Option<FaultInfo>,
}

impl InteractFault {
    pub fn in_src(fi: impl Into<FaultInfo>) -> Self {
        Self {
            src: Some(fi.into()),
            dst: None,
        }
    }

    pub fn in_dst(fi: impl Into<FaultInfo>) -> Self {
        Self {
            src: None,
            dst: Some(fi.into()),
        }
    }

    /// Discharges the `src` side of this fault, if any, by forcing it on the
    /// given task. Returns the `dst` side.
    ///
    /// This is intended to be called during syscalls from the recipient's
    /// perspective, to store the src fault and then deal with dst.
    pub fn apply_to_src(self, src: &mut Task) -> Result<(), FaultInfo> {
        if let Some(f) = self.src {
            let _ = src.force_fault(f);
        }
        if let Some(f) = self.dst {
            Err(f)
        } else {
            Ok(())
        }
    }

    /// Discharges the `dst` side of this fault, if any, by forcing it on the
    /// given task. Returns the `src` side.
    ///
    /// This is intended to be called during syscalls from the sender's
    /// perspective, to store the dst fault and then deal with dst.
    pub fn apply_to_dst(self, dst: &mut Task) -> Result<(), FaultInfo> {
        if let Some(f) = self.dst {
            let _ = dst.force_fault(f);
        }
        if let Some(f) = self.src {
            Err(f)
        } else {
            Ok(())
        }
    }
}

