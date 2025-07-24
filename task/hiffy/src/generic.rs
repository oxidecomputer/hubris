// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use hif::Function;
use hubris_num_tasks::Task;

// This type is only used by the debugger apparently, so its field appears
// unused to the compiler.
pub struct Buffer(#[allow(dead_code)] u8);

pub enum Functions {
    Sleep(u16, u32),
    Send((Task, u16, Buffer, usize), u32),
    SendLeaseRead((Task, u16, Buffer, usize, usize), u32),
    SendLeaseWrite((Task, u16, Buffer, usize, usize), u32),
}

#[no_mangle]
#[used(compiler)]
static HIFFY_FUNCTIONS: Option<&Functions> = None;

pub(crate) static HIFFY_FUNCS: &[Function] = &[
    crate::common::sleep,
    crate::common::send,
    crate::common::send_lease_read,
    crate::common::send_lease_write,
];

pub(crate) fn trace_execute(_offset: usize, _op: hif::Op) {}

pub(crate) fn trace_success() {}

pub(crate) fn trace_failure(_f: hif::Failure) {}
