use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
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
    if let Ok(pkg_map) = env::var("HUBRIS_PKG_MAP") {
        let map: serde_json::Value = serde_json::from_str(&pkg_map).unwrap();
        let map = map.as_object().unwrap();

        // Put the linker script somewhere the linker can find it
        let mut linkscr = File::create(out.join("memory.x")).unwrap();
        writeln!(linkscr, "MEMORY\n{{").unwrap();
        for (name, range) in map {
            println!("{:?}", range);
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
    println!("cargo:rerun-if-env-changed=HUBRIS_PKG_MAP");
}
