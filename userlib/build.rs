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

    // Put the linker script somewhere the linker can find it
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    File::create(out.join("link.x"))
        .unwrap()
        .write_all(include_bytes!("link.x"))
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    // Only re-run the build script when link.x is changed,
    // instead of when any part of the source code changes.
    println!("cargo:rerun-if-changed=link.x");

    // Generate our memory include from the environment if possible.
    println!("cargo:rerun-if-env-changed=HUBRIS_PKG_MAP");
    if let Ok(pkg_map) = env::var("HUBRIS_PKG_MAP") {
        println!("HUBRIS_PKG_MAP = {:#x?}", pkg_map);
        let map: serde_json::Value = serde_json::from_str(&pkg_map).unwrap();
        let map = map.as_object().unwrap();

        // Put the linker script somewhere the linker can find it
        let mut linkscr = File::create(out.join("memory.x")).unwrap();
        writeln!(linkscr, "MEMORY\n{{").unwrap();
        for (name, range) in map {
            let start = range["start"].as_u64().unwrap();
            let end = range["end"].as_u64().unwrap();
            let name = name.to_ascii_uppercase();
            writeln!(linkscr, "{} (rwx) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}", name, start, end - start).unwrap();
        }
        write!(linkscr, "}}").unwrap();
        drop(linkscr);

    } else {
        // We're building outside the context of an image. Generate a
        // placeholder memory layout.
        let mut linkscr = File::create(out.join("memory.x")).unwrap();
        writeln!(linkscr, "\
            MEMORY {{\n\
                FLASH (rx) : ORIGIN = 0x00000000, LENGTH = 128K\n\
                RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 128K\n\
            }}").unwrap();
        drop(linkscr);
    }

    println!("cargo:rerun-if-env-changed=HUBRIS_TASKS");
    println!("cargo:rerun-if-env-changed=HUBRIS_TASK_SELF");
    let mut task_enum = vec![];
    let task_self;
    if let Ok(task_names) = env::var("HUBRIS_TASKS") {
        println!("HUBRIS_TASKS = {}", task_names);
        task_self = env::var("HUBRIS_TASK_SELF").unwrap();
        println!("HUBRIS_TASK_SELF = {}", task_self);
        for (i, name) in task_names.split(",").enumerate() {
            task_enum.push(format!("    {} = {},", name, i));
        }
    } else {
        task_enum.push("    anonymous = 0,".to_string());
        task_self = "anonymous".to_string();
    }
    let mut task_file = std::fs::File::create(out.join("tasks.rs"))?;
    writeln!(task_file, "#[allow(non_camel_case_types)]")?;
    writeln!(task_file, "pub enum Task {{")?;
    for line in task_enum {
        writeln!(task_file, "{}", line)?;
    }
    writeln!(task_file, "}}")?;
    writeln!(task_file, "pub const SELF: Task = Task::{};", task_self)?;

    Ok(())
}
