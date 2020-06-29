use std::error::Error;
use std::path::PathBuf;
use std::process::Command;

pub fn run(gdb_cfg: PathBuf) -> Result<(), Box<dyn Error>> {
    let mut cmd = Command::new("arm-none-eabi-gdb");
    cmd.arg("-q")
        .arg("-x")
        .arg("target/dist/script.gdb")
        .arg("-x")
        .arg(&gdb_cfg)
        .arg("target/dist/combined.elf");

    let status = cmd.status()?;
    if !status.success() {
        return Err("command failed, see output for details".into());
    }

    Ok(())
}
