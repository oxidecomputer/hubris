use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Config;

pub fn run(cfg: &Path, gdb_cfg: &Path) -> Result<(), Box<dyn Error>> {
    ctrlc::set_handler(|| {}).expect("Error setting Ctrl-C handler");

    let cfg_contents = std::fs::read(&cfg)?;
    let toml: Config = toml::from_slice(&cfg_contents)?;

    let mut out = PathBuf::from("target");
    out.push(toml.name);
    out.push("dist");

    let gdb_path = out.join("script.gdb");
    let combined_path = out.join("combined.elf");

    let mut cmd = Command::new("arm-none-eabi-gdb");
    cmd.arg("-q")
        .arg("-x")
        .arg(gdb_path)
        .arg("-x")
        .arg(&gdb_cfg)
        .arg(combined_path);

    let status = cmd.status()?;
    if !status.success() {
        return Err("command failed, see output for details".into());
    }

    Ok(())
}
