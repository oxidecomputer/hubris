// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fs::File;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = build_util::out_dir();

    let task_env = build_util::env_var("HUBRIS_TASKS").unwrap_or_else(|_| {
        panic!("can't build this crate outside of the build system.")
    });

    let task_names = task_env.split(',').collect::<Vec<_>>();
    let count = task_names.len();

    let mut task_file = File::create(out.join("tasks.rs")).unwrap();
    writeln!(task_file, "pub static TASK_NAMES: [&str; {count}] = [").unwrap();
    for name in &task_names {
        writeln!(task_file, "    {name:?},").unwrap();
    }
    writeln!(task_file, "];").unwrap();

    let longest = task_names.iter().map(|s| s.len()).max().unwrap_or(0);
    writeln!(task_file, "pub const MAX_TASK_NAME: usize = {longest};").unwrap();

    Ok(())
}
