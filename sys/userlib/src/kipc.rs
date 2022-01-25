// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Operations implemented by IPC with the kernel task.

use unwrap_lite::UnwrapLite;
use zerocopy::AsBytes;

use crate::*;

pub fn read_task_status(task: usize) -> abi::TaskState {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let mut response = [0; core::mem::size_of::<abi::TaskState>()];
    let (rc, len) =
        sys_send(TaskId::KERNEL, 1, task.as_bytes(), &mut response, &[]);
    assert_eq!(rc, 0);
    ssmarshal::deserialize(&response[..len]).unwrap_lite().0
}

pub fn restart_task(task: usize, start: bool) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let msg = (task as u32, start);
    let mut buf = [0; core::mem::size_of::<(u32, bool)>()];
    ssmarshal::serialize(&mut buf, &msg).unwrap_lite();
    let (rc, _len) = sys_send(TaskId::KERNEL, 2, &mut buf, &mut [], &[]);
    assert_eq!(rc, 0);
}

pub fn fault_task(task: usize) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let (rc, _len) = sys_send(TaskId::KERNEL, 3, task.as_bytes(), &mut [], &[]);
    assert_eq!(rc, 0);
}
