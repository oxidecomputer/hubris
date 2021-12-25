// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use hif::Function;
use hubris_num_tasks::Task;

pub struct Buffer(u8);

pub enum Functions {
    Sleep(u16, u32),
    Send((Task, u16, Buffer, usize), u32),
}

#[no_mangle]
static HIFFY_FUNCTIONS: Option<&Functions> = None;

pub(crate) static HIFFY_FUNCS: &[Function] = &[
    crate::common::sleep,
    crate::common::send,
];

pub(crate) fn trace_execute(_offset: usize, _op: hif::Op) {}

pub(crate) fn trace_success() {}

pub(crate) fn trace_failure(_f: hif::Failure) {}
