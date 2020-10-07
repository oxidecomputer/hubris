use std::env;
use std::path::Path;
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

    let target = target.unwrap_or_else(|| {
        let path = env::current_dir().unwrap();
        let manifest_path = path.join("Cargo.toml");

        get_target(&manifest_path).unwrap()
    });

    println!("checking: {}", package);

    let mut cmd = Command::new("cargo");
    cmd.arg("check");
    cmd.arg("-p");
    cmd.arg(&package);
    cmd.arg("--target");
    cmd.arg(target);

    // this is only actually used for demo-stm32h7 but is harmless to include, so let's do
    // it unconditionally
    cmd.env("HUBRIS_BOARD", "nucleo-h743zi2");

    let status = cmd.status()?;

    if !status.success() {
        bail!("Could not build {}", package);
    }

    Ok(())
}

fn get_target(manifest_path: &Path) -> Result<String> {
    let contents = std::fs::read(manifest_path)?;
    let toml: toml::Value = toml::from_slice(&contents)?;

    // someday, try blocks will be stable...
    let target = (|| {
        Some(
            toml.get("package")?
                .get("metadata")?
                .get("build")?
                .get("target")?
                .as_str()?,
        )
    })();

    match target {
        Some(target) => Ok(target.to_string()),
        None => bail!("Could not find target, please set [package.metadata.build.target] in Cargo.toml"),
    }
}
