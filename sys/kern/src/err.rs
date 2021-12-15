// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common error-handling support.
//!
//! This module is designed around the idea that kernel code spends too much
//! time handling and recording errors, and we ought to be able to separate that
//! concern using `Result`.

use abi::{FaultInfo, UsageError};

use crate::task::{self, NextTask, Task};

/// An error committed by user code when interacting with a syscall.
///
/// This is used internally as the returned error type for syscall
/// implementations.
#[derive(Clone, Debug)]
pub enum UserError {
    /// A recoverable error. Recoverable errors are indicated to the errant task
    /// by returning a response code (the `u32` field). They may still cause a
    /// context switch, however, as indicated by the `NextTask`.
    Recoverable(u32, NextTask),
    /// An unrecoverable error. Unrecoverable errors are translated to faults
    /// against the errant task, which is marked faulted and no longer runnable.
    Unrecoverable(FaultInfo),
}

/// Convenience conversion from `FaultInfo`.
impl From<FaultInfo> for UserError {
    fn from(f: FaultInfo) -> Self {
        Self::Unrecoverable(f)
    }
}

/// Convenience conversion from `UsageError` (by way of `FaultInfo`).
impl From<UsageError> for UserError {
    fn from(f: UsageError) -> Self {
        Self::Unrecoverable(f.into())
    }
}

/// A fault that arose in the interaction between two tasks (i.e. during message
/// transfer).
///
/// This can assign fault to either or both tasks. By convention, an
/// `InteractFault` won't contain both fields as `None`, though the type system
/// doesn't prevent this.
#[derive(Copy, Clone, Debug)]
pub struct InteractFault {
    /// Fault in the source task of a transfer.
    pub src: Option<FaultInfo>,
    /// Fault in the destination task of a transfer.
    pub dst: Option<FaultInfo>,
}

impl InteractFault {
    /// Convenience mapping to take a `FaultInfo`, or something that can become
    /// one, and turn it into an `InteractFault` blaming the source.
    pub fn in_src(fi: impl Into<FaultInfo>) -> Self {
        Self {
            src: Some(fi.into()),
            dst: None,
        }
    }

    /// Convenience mapping to take a `FaultInfo`, or something that can become
    /// one, and turn it into an `InteractFault` blaming the destination.
    pub fn in_dst(fi: impl Into<FaultInfo>) -> Self {
        Self {
            src: None,
            dst: Some(fi.into()),
        }
    }

    /// Discharges the `src` side of this fault, if any, by forcing it on the
    /// given task. Returns the `dst` side.
    ///
    /// This is intended to be called during syscalls from the destination's
    /// perspective, to store the src fault and then deal with dst.
    pub fn apply_to_src(
        self,
        tasks: &mut [Task],
        src: usize,
    ) -> Result<task::NextTask, FaultInfo> {
        let nt = if let Some(f) = self.src {
            task::force_fault(tasks, src, f)
        } else {
            task::NextTask::Same
        };
        if let Some(f) = self.dst {
            Err(f)
        } else {
            Ok(nt)
        }
    }

    /// Discharges the `dst` side of this fault, if any, by forcing it on the
    /// given task. Returns the `src` side.
    ///
    /// This is intended to be called during syscalls from the source's
    /// perspective, to store the dst fault and then deal with dst.
    pub fn apply_to_dst(
        self,
        tasks: &mut [Task],
        dst: usize,
    ) -> Result<task::NextTask, FaultInfo> {
        let nt = if let Some(f) = self.dst {
            task::force_fault(tasks, dst, f)
        } else {
            task::NextTask::Same
        };
        if let Some(f) = self.src {
            Err(f)
        } else {
            Ok(nt)
        }
    }
}
