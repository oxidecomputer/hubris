use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    linker_script_plumbing();
    build_assembly_sources();
}

fn build_assembly_sources() {
    cc::Build::new()
        .file("asm/sys.S")
        .compile("libunrusted.a");
    println!("cargo:rerun-if-changed=asm/sys.S");
}

fn linker_script_plumbing() {
    println!("cargo:rerun-if-changed=link.x");
}
