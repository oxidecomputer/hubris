use std::error::Error;
use std::path::Path;
use std::process::Command;

pub fn run(path: &Path, target: &str) -> Result<(), Box<dyn Error>> {
    println!("building: {}", path.display());

    // execute our build
    Command::new("cargo")
        .env("RUSTFLAGS", "-C link-arg=-Tlink.x")
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg(target)
        .status()?;

    Ok(())
}
