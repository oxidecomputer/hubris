use std::env;
use std::process::Command;

use anyhow::{bail, Result};

fn get_custom_target(pkg_name: &str) -> Option<String> {
    use cargo_metadata::MetadataCommand;
    use serde::Deserialize;

    let metadata = MetadataCommand::new()
        .exec()
        .unwrap();

    let package = metadata
        .packages
        .iter()
        .find(|p| p.name == pkg_name)
        .unwrap()
        .clone();

    #[derive(Debug, Deserialize)]
    struct CustomMetadata {
        build: Option<BuildMetadata>,
    }

    #[derive(Debug, Deserialize)]
    struct BuildMetadata {
        target: Option<String>,
    }

    let m: Option<CustomMetadata> =
        serde_json::from_value(package.metadata).unwrap();

    (|| m?.build?.target)()
}

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
            .expect("Couldn't find [package.name]; pass -p <package> to check a specific package or --all to check all packages")
            .to_string()
    });

    let target = target.or(get_custom_target(&package));

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
