//! Operations implemented by IPC with the kernel task.

use zerocopy::AsBytes;

use crate::*;

pub fn read_task_status(task: usize) -> abi::TaskState {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let mut response = [0; core::mem::size_of::<abi::TaskState>()];
    let (rc, len) = sys_send(TaskId::KERNEL, 1, task.as_bytes(), &mut response, &[]);
    assert_eq!(rc, 0);
    ssmarshal::deserialize(&response[..len])
        .map_err(|_| ())
        .unwrap()
        .0
}
