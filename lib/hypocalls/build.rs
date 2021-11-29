// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Do an architecture check.
    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "none" {
        eprintln!("***********************************************");
        eprintln!("Hi!");
        eprintln!("You appear to be building this natively,");
        eprintln!("i.e. for your workstation. This won't work.");
        eprintln!("Please specify --target=some-triple, e.g.");
        eprintln!("--target=thumbv7em-none-eabihf");
        eprintln!("***********************************************");
        panic!()
    }

    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target_dir = &PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut task_file = File::create(out.join("hypo.rs")).unwrap();
    // This contains the addresses of the secure entry points so make sure
    // this crate gets rebuilt if it changes
    println!(
        "cargo:rerun-if-changed={:?}",
        target_dir.join("../target/table.ld")
    );
    if let Ok(shared_syms) = env::var("SHARED_SYMS") {
        writeln!(task_file, "#[repr(C)]").unwrap();
        writeln!(task_file, "pub struct BootloaderSyms {{").unwrap();
        for s in shared_syms.split(",") {
            writeln!(task_file, "    pub {} : usize,", s).unwrap();
        }
        writeln!(task_file, "}}").unwrap();
        writeln!(task_file, "\nextern \"C\" {{").unwrap();
        writeln!(
            task_file,
            "    pub static __bootloader_fn_table: BootloaderSyms;"
        )
        .unwrap();
        writeln!(task_file, "}}").unwrap();
    }

    Ok(())
}
