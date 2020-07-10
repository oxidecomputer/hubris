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
    build_util::generate_hubris_task_linker_script();

    build_util::generate_hubris_task_includes();

    Ok(())
}
