use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

/// Exposes the CPU's M-profile architecture version. This isn't available in
/// rustc's standard environment.
///
/// This will set either `cfg(armv7m)` or `cfg(armv8m)` depending on the value
/// of the `TARGET` environment variable.
pub fn expose_m_profile() {
    let target = env::var("TARGET").unwrap();

    if target.starts_with("thumbv7m") || target.starts_with("thumbv7em") {
        println!("cargo:rustc-cfg=armv7m");
    } else if target.starts_with("thumbv8m") {
        println!("cargo:rustc-cfg=armv8m");
    } else {
        println!("Don't know the target {}", target);
        std::process::exit(1);
    }
}

/// Exposes the board type from the `HUBRIS_BOARD` envvar into
/// `cfg(target_board="...")`.
pub fn expose_target_board() {
    if let Ok(board) = env::var("HUBRIS_BOARD") {
        println!("cargo:rustc-cfg=target_board=\"{}\"", board);
    }
    println!("cargo:rerun-if-env-changed=HUBRIS_BOARD");
}

/// Generates the linker script for a kernel stub program.
///
/// The linker script goes into `OUT_DIR/link.x` and `OUT_DIR` is added to the
/// linker search path.
///
/// This is controlled by two environment variables:
/// - `HUBRIS_PKG_MAP` defines the memory layout for the task.
/// - `HUBRIS_DESCRIPTOR` contains the full application descriptor as literals.
///
/// (TODO: should also explain the _contents_ of those vars.)
///
/// If these variables are not set, this generates a default linker script for
/// standalone builds.
pub fn generate_hubris_kernel_linker_script() {
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());

    if let Ok(map) = env::var("HUBRIS_PKG_MAP") {
        let map: serde_json::Value = serde_json::from_str(&map).unwrap();
        let map = map.as_object().unwrap();

        // Put the linker script somewhere the linker can find it
        let mut linkscr = File::create(out.join("memory.x")).unwrap();
        writeln!(linkscr, "MEMORY\n{{").unwrap();
        for (name, range) in map {
            println!("{:?}", range);
            let start = range["start"].as_u64().unwrap();
            let end = range["end"].as_u64().unwrap();
            let name = name.to_ascii_uppercase();
            writeln!(
                linkscr,
                "{} (rwx) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}",
                name,
                start,
                end - start
            )
            .unwrap();
        }
        writeln!(linkscr, "}}").unwrap();
        writeln!(linkscr, "__eheap = ORIGIN(RAM) + LENGTH(RAM);").unwrap();
        writeln!(linkscr, "SECTIONS {{").unwrap();
        writeln!(linkscr, "  .hubris_app_table : AT(__erodata) {{").unwrap();
        writeln!(linkscr, "    hubris_app_table = .;").unwrap();
        writeln!(linkscr, "{}", env::var("HUBRIS_DESCRIPTOR").unwrap())
            .unwrap();
        writeln!(linkscr, "  }} > FLASH").unwrap();
        writeln!(linkscr, "}} INSERT AFTER .data").unwrap();
        drop(linkscr);
    } else {
        // Generate a placeholder version.
        let mut linkscr = File::create(out.join("memory.x")).unwrap();
        writeln!(
            linkscr,
            "\
            MEMORY {{\n\
                FLASH (rx) : ORIGIN = 0x00000000, LENGTH = 128K\n\
                RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 128K\n\
            }}"
        )
        .unwrap();
        writeln!(linkscr, "__eheap = ORIGIN(RAM) + LENGTH(RAM);").unwrap();
        writeln!(linkscr, "SECTIONS {{").unwrap();
        writeln!(linkscr, "  .hubris_app_table : AT(__erodata) {{").unwrap();
        writeln!(linkscr, "    hubris_app_table = .;").unwrap();
        writeln!(linkscr, "    . += 32;").unwrap();
        writeln!(linkscr, "  }} > FLASH").unwrap();
        writeln!(linkscr, "}} INSERT AFTER .data").unwrap();
        drop(linkscr);
    }
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-env-changed=HUBRIS_PKG_MAP");
    println!("cargo:rerun-if-env-changed=HUBRIS_DESCRIPTOR");
}

/// Generates the linker script for a particular instance of a task.
///
/// The linker script goes into `OUT_DIR/memory.x`, which `link.x` should
/// include. `OUT_DIR` is added to the linker search path.
///
/// The generation of the script is controlled by the `HUBRIS_PKG_MAP` variable,
/// whose contents should really be explained here (TODO).
///
/// If that variable is not set, generates a default placeholder script.
pub fn generate_hubris_task_linker_script() {
    // TODO: this could be refactored to share code with the kernel script
    // above!
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
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
            writeln!(
                linkscr,
                "{} (rwx) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}",
                name,
                start,
                end - start
            )
            .unwrap();
        }
        write!(linkscr, "}}").unwrap();
        drop(linkscr);
    } else {
        // We're building outside the context of an image. Generate a
        // placeholder memory layout.
        let mut linkscr = File::create(out.join("memory.x")).unwrap();
        writeln!(
            linkscr,
            "\
            MEMORY {{\n\
                FLASH (rx) : ORIGIN = 0x00000000, LENGTH = 128K\n\
                RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 128K\n\
            }}"
        )
        .unwrap();
        drop(linkscr);
    }
}

/// Generates an `OUT_DIR/tasks.rs` file containing the set of tasks in the
/// application as viewed from the current task.
///
/// This relies on the `HUBRIS_TASKS` and `HUBRIS_TASK_SELF` environment
/// variables.
/// - `HUBRIS_TASKS` should be a comma-separated list of task names, in order of
///   index.
/// - `HUBRIS_TASK_SELF` should be the name of the task being compiled.
///
/// If not provided, generates a default bogus `Task` enum.
pub fn generate_hubris_task_includes() {
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=HUBRIS_TASKS");
    println!("cargo:rerun-if-env-changed=HUBRIS_TASK_SELF");
    let mut task_enum = vec![];
    let task_self;
    let task_count;
    if let Ok(task_names) = env::var("HUBRIS_TASKS") {
        println!("HUBRIS_TASKS = {}", task_names);
        task_self = env::var("HUBRIS_TASK_SELF").unwrap();
        println!("HUBRIS_TASK_SELF = {}", task_self);
        for (i, name) in task_names.split(",").enumerate() {
            task_enum.push(format!("    {} = {},", name, i));
        }
        task_count = task_names.split(",").count();
    } else {
        task_enum.push("    anonymous = 0,".to_string());
        task_self = "anonymous".to_string();
        task_count = 1;
    }
    let mut task_file = std::fs::File::create(out.join("tasks.rs")).unwrap();
    writeln!(task_file, "#[allow(non_camel_case_types)]").unwrap();
    writeln!(task_file, "pub enum Task {{").unwrap();
    for line in task_enum {
        writeln!(task_file, "{}", line).unwrap();
    }
    writeln!(task_file, "}}").unwrap();
    writeln!(task_file, "pub const SELF: Task = Task::{};", task_self).unwrap();
    writeln!(task_file, "pub const NUM_TASKS: usize = {};", task_count)
        .unwrap();
}
