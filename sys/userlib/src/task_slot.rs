// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use abi::{Generation, TaskId};

/// TaskSlot index mapping.
pub struct TaskSlot(u16);

impl TaskSlot {
    /// Parse a slot index from the given string, e.g. "11".
    ///
    /// The mapping is provided via an `env!` var inserted by `xtask`.
    /// See the `task_slot!` macro for more details.
    ///
    /// This is intended to (only) be used at compile time, and panics if
    /// `index` cannot be parsed as a `u16`.
    pub const fn parse(index: &'static str) -> Self {
        Self(parse_index(index))
    }

    pub fn get_task_id(&self) -> TaskId {
        let task_index = self.get_task_index();

        let prototype =
            TaskId::for_index_and_gen(task_index.into(), Generation::default());
        crate::sys_refresh_task_id(prototype)
    }

    pub fn get_task_index(&self) -> u16 {
        self.0
    }
}

/// const-fn version of `str::parse::<u16>().unwrap()`/`atoi`.
///
/// Panics if parsing is not successful.
const fn parse_index(text: &str) -> u16 {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        panic!("empty task slot index");
    }
    let mut value: u16 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let digit = bytes[i];
        if !digit.is_ascii_digit() {
            panic!("task slot index is not a decimal number");
        }
        value = match value.checked_mul(10) {
            Some(v) => match v.checked_add((digit - b'0') as u16) {
                Some(v) => v,
                None => panic!("task slot index is out of range"),
            },
            None => panic!("task slot index is out of range"),
        };
        i += 1;
    }
    value
}
