//! Operations implemented by IPC with the kernel task.

use zerocopy::AsBytes;

use crate::*;

pub fn read_task_status(task: usize) -> abi::TaskState {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let mut response = [0; core::mem::size_of::<abi::TaskState>()];
    let (rc, len) =
        sys_send(TaskId::KERNEL, 1, task.as_bytes(), &mut response, &[]);
    assert_eq!(rc, 0);
    ssmarshal::deserialize(&response[..len])
        .map_err(|_| ())
        .unwrap()
        .0
}

pub fn restart_task(task: usize, start: bool) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let msg = (task as u32, start);
    let mut buf = [0; core::mem::size_of::<(u32, bool)>()];
    ssmarshal::serialize(&mut buf, &msg)
        .map_err(|_| ())
        .unwrap();
    let (rc, _len) = sys_send(TaskId::KERNEL, 2, &mut buf, &mut [], &[]);
    assert_eq!(rc, 0);
}

pub fn fault_task(task: usize) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let (rc, _len) = sys_send(TaskId::KERNEL, 3, task.as_bytes(), &mut [], &[]);
    assert_eq!(rc, 0);
}
