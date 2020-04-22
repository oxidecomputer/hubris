use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());

    // Put the linker script somewhere the linker can find it
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(include_bytes!("memory.x"))
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    // Only re-run the build script when memory.x is changed,
    // instead of when any part of the source code changes.
    println!("cargo:rerun-if-changed=memory.x");

    // Guess at the path to the task binaries.
    // Typical out path: target/thumbv7em-none-eabihf/debug/build/demo-d8561f9daeb4e6d3/out
    let bindir = out.parent().unwrap().parent().unwrap().parent().unwrap();

    let task_ping = bindir.join("task-ping");
    let task_ping_bin = out.join("task_ping.bin");
    let task_ping_hex = out.join("task_ping.hex");
    extract_binary(&task_ping, &task_ping_bin);
    write_hex_literal(&task_ping_bin, &task_ping_hex);

    let task_pong = bindir.join("task-pong");
    let task_pong_bin = out.join("task_pong.bin");
    let task_pong_hex = out.join("task_pong.hex");
    extract_binary(&task_pong, &task_pong_bin);
    write_hex_literal(&task_pong_bin, &task_pong_hex);

    println!("cargo:rerun-if-changed={}", task_ping.display());
    println!("cargo:rerun-if-changed={}", task_pong.display());

    println!("cargo:rustc-env=TASK_PING_PATH={}", task_ping_hex.display());
    println!("cargo:rustc-env=TASK_PONG_PATH={}", task_pong_hex.display());
}

fn extract_binary(input: &std::path::Path, output: &std::path::Path) {
    let status = Command::new("arm-none-eabi-objcopy")
        .arg(input)
        .arg("-Obinary")
        .arg(output)
        .status()
        .unwrap();

    assert!(status.success());
}

fn write_hex_literal(input: &std::path::Path, output: &std::path::Path) {
    let bytes = std::fs::read(input).unwrap();
    let mut f = std::fs::File::create(output).unwrap();
    write!(f, "[").unwrap();
    let n = bytes.len();
    for b in bytes {
        write!(f, "0x{:02x},", b).unwrap();
    }
    for _ in n..16384 {
        write!(f, "0,").unwrap();
    }
    write!(f, "]").unwrap();
}
