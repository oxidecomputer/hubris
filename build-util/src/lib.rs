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
