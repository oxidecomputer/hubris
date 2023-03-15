// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Operations implemented by IPC with the kernel task.

use crate::UnwrapLite;
use zerocopy::AsBytes;

use crate::*;

pub fn read_task_status(task: usize) -> abi::TaskState {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let mut response = [0; core::mem::size_of::<abi::TaskState>()];
    let (rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadTaskStatus as u16,
        task.as_bytes(),
        &mut response,
        &[],
    );
    assert_eq!(rc, 0);
    ssmarshal::deserialize(&response[..len]).unwrap_lite().0
}

pub fn read_task_dump_region(
    task: usize,
    region: usize,
) -> Option<abi::TaskDumpRegion> {
    let msg = (task as u32, region as u32);
    let mut buf = [0; core::mem::size_of::<(u32, u32)>()];
    ssmarshal::serialize(&mut buf, &msg).unwrap_lite();

    let mut response = [0; core::mem::size_of::<Option<abi::TaskDumpRegion>>()];
    let (rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadTaskDumpRegion as u16,
        &buf,
        &mut response,
        &[],
    );
    assert_eq!(rc, 0);
    ssmarshal::deserialize(&response[..len]).unwrap_lite().0
}

pub fn read_task(
    task: usize,
    region: abi::TaskDumpRegion,
    response: &mut [u8],
) -> usize {
    let msg = (task as u32, region);
    let mut buf = [0; core::mem::size_of::<(u32, abi::TaskDumpRegion)>()];
    ssmarshal::serialize(&mut buf, &msg).unwrap_lite();

    let (rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadTask as u16,
        &buf,
        response,
        &[],
    );
    assert_eq!(rc, 0);
    len
}

pub fn restart_task(task: usize, start: bool) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let msg = (task as u32, start);
    let mut buf = [0; core::mem::size_of::<(u32, bool)>()];
    ssmarshal::serialize(&mut buf, &msg).unwrap_lite();
    let (rc, _len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::RestartTask as u16,
        &buf,
        &mut [],
        &[],
    );
    assert_eq!(rc, 0);
}

pub fn fault_task(task: usize) {
    // Coerce `task` to a known size (Rust doesn't assume that usize == u32)
    let task = task as u32;
    let (rc, _len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::FaultTask as u16,
        task.as_bytes(),
        &mut [],
        &[],
    );
    assert_eq!(rc, 0);
}

pub fn system_restart() -> ! {
    let _ = sys_send(TaskId::KERNEL, Kipcnum::Reset as u16, &[], &mut [], &[]);
    panic!();
}

pub fn read_image_id() -> u64 {
    let mut response = [0; core::mem::size_of::<u64>()];
    let (rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadImageId as u16,
        &[],
        &mut response,
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 8); // we *really* expect this to be a u64
    ssmarshal::deserialize(&response[..len]).unwrap_lite().0
}

/// Returns the position of the caboose in memory, or an empty range [0,0)
///
/// This is a low-level KIPC function; [`get_caboose`] is the more useful
/// wrapper that returns a meaningful value.
fn read_caboose_pos() -> core::ops::Range<u32> {
    let mut response = [0; core::mem::size_of::<core::ops::Range<u32>>()];
    let (rc, len) = sys_send(
        TaskId::KERNEL,
        Kipcnum::ReadCaboosePos as u16,
        &[],
        &mut response,
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 8); // we *really* expect this to be a (u32, u32)
    ssmarshal::deserialize(&response[..len]).unwrap_lite().0
}

/// Returns the caboose as a static slice, or `None` if no caboose is present
pub fn get_caboose() -> Option<&'static [u8]> {
    let region = read_caboose_pos();
    if region.start == 0 && region.end == 0 {
        None
    } else {
        // SAFETY: these values are given to us by the kernel, and should point
        // to a region in flash memory that does not exceed the bounds of flash.
        unsafe {
            Some(core::slice::from_raw_parts(
                region.start as *const u8,
                region.len(),
            ))
        }
    }
}
