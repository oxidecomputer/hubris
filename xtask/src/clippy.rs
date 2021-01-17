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
        toml.get("package")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .expect("Couldn't find [package.name]; pass -p <package> to run clippy on a specific package or --all to check all packages")
            .to_string()
    });

    println!("Running clippy on: {}", package);

    let mut cmd = Command::new("cargo");
    cmd.arg("clippy");
    // TODO: Remove this argument once resolved on stable:
    //
    // https://github.com/rust-lang/rust-clippy/issues/4612
    //
    // TL;DR: Switching back and forth between "clippy" and "check"
    // caches the results from one execution. This means a succeeding
    // "check" execution may hide a failing "clippy" execution.
    cmd.arg("-Zunstable-options");
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
