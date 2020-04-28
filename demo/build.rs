use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
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
            writeln!(linkscr, "{} (rwx) : ORIGIN = 0x{:08x}, LENGTH = 0x{:08x}", name, start, end - start).unwrap();
        }
        writeln!(linkscr, "}}").unwrap();
        writeln!(linkscr, "__eheap = ORIGIN(RAM) + LENGTH(RAM);").unwrap();
        writeln!(linkscr, "SECTIONS {{").unwrap();
        writeln!(linkscr, "  .hubris_app_table : AT(__erodata) {{").unwrap();
        writeln!(linkscr, "    hubris_app_table = .;").unwrap();
        writeln!(linkscr, "{}", env::var("HUBRIS_DESCRIPTOR").unwrap()).unwrap();
        writeln!(linkscr, "  }} > FLASH").unwrap();
        writeln!(linkscr, "}} INSERT AFTER .data").unwrap();
        drop(linkscr);
    } else {
        // Generate a placeholder version.
        let mut linkscr = File::create(out.join("memory.x")).unwrap();
        writeln!(linkscr, "\
            MEMORY {{\n\
                FLASH (rx) : ORIGIN = 0x00000000, LENGTH = 128K\n\
                RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 128K\n\
            }}").unwrap();
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
