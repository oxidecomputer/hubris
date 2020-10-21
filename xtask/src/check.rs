use std::env;
use std::process::Command;

use anyhow::{bail, Result};

pub fn run(package: Option<String>, target: Option<String>) -> Result<()> {
    let package = package.unwrap_or_else(|| {
        let path = env::current_dir().unwrap();
        let manifest_path = path.join("Cargo.toml");
        let contents = std::fs::read(manifest_path).unwrap();
        let toml: toml::Value = toml::from_slice(&contents).unwrap();

        // someday, try blocks will be stable...
        let package = (|| {
            Some(
                toml.get("package")
                    .unwrap()
                    .get("name")
                    .unwrap()
                    .as_str()
                    .unwrap(),
            )
        })();

        package.unwrap().to_string()
    });

    println!("checking: {}", package);

    let mut cmd = Command::new("cargo");
    cmd.arg("check");
    cmd.arg("-p");
    cmd.arg(&package);

    if target.is_some() {
        cmd.arg("--target");
        cmd.arg(target.unwrap());
    }

    // this is only actually used for demo-stm32h7 but is harmless to include, so let's do
    // it unconditionally
    cmd.env("HUBRIS_BOARD", "nucleo-h743zi2");

    let status = cmd.status()?;

    if !status.success() {
        bail!("Could not build {}", package);
    }

    Ok(())
}
