// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fs::File;
use std::io::Write;

const MICROCBOR: &str = "microcbor";
const SERDE: &str = "serde";
const HUBPACK: &str = "hubpack";
const TASK_ENUM: &str = "task-enum";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = build_util::out_dir();

    let mut task_enum = vec![];
    if let Ok(task_names) = build_util::env_var("HUBRIS_TASKS") {
        println!("HUBRIS_TASKS = {task_names}",);
        for (i, name) in task_names.split(',').enumerate() {
            task_enum.push((i, name.to_owned()));
        }
    } else {
        panic!("can't build this crate outside of the build system.");
    }
    let task_count = task_enum.len();

    let mut task_file = File::create(out.join("tasks.rs")).unwrap();

    if build_util::has_feature(TASK_ENUM) {
        // Generate various derives, as requested.
        if build_util::has_feature(MICROCBOR) {
            writeln!(task_file, "#[derive(microcbor::Encode)]").unwrap();
        }
        if build_util::has_feature(SERDE) {
            writeln!(
                task_file,
                "#[derive(serde::Serialize, serde::Deserialize)]"
            )
            .unwrap();
        }
        if build_util::has_feature(HUBPACK) {
            writeln!(task_file, "#[derive(hubpack::SerializedSize)]").unwrap();
        }
        writeln!(task_file, "#[derive(Copy, Clone, Eq, PartialEq)]").unwrap();
        writeln!(task_file, "#[allow(non_camel_case_types)]").unwrap();
        writeln!(task_file, "pub enum Task {{").unwrap();
        for (i, name) in &task_enum {
            writeln!(task_file, "    {name} = {i},").unwrap();
        }
        writeln!(task_file, "}}").unwrap();
        writeln!(task_file).unwrap();
        writeln!(task_file, "impl TryFrom<usize> for Task {{").unwrap();
        writeln!(task_file, "    type Error = ();").unwrap();
        writeln!(
            task_file,
            "    fn try_from(u: usize) -> Result<Self, Self::Error> {{"
        )
        .unwrap();
        writeln!(task_file, "        match u {{").unwrap();
        for (i, name) in &task_enum {
            writeln!(task_file, "            {i} => Ok(Self::{name}),")
                .unwrap();
        }
        writeln!(task_file, "            _ => Err(()),").unwrap();
        writeln!(task_file, "        }}").unwrap();
        writeln!(task_file, "    }}").unwrap();
        writeln!(task_file, "}}").unwrap();
    } else if build_util::has_feature(MICROCBOR)
        || build_util::has_feature(SERDE)
        || build_util::has_feature(HUBPACK)
    {
        println!(
            "cargo::warning=the `hubris-num-tasks` feature flags \
             {MICROCBOR:?}, {SERDE:?} and {HUBPACK:?} do nothing if the \
             {TASK_ENUM:?} feature is not enabled."
        );
    }

    writeln!(task_file, "pub const NUM_TASKS: usize = {task_count};").unwrap();

    Ok(())
}
