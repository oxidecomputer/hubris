use std::env;

use serde::Deserialize;
use indexmap::IndexMap;

/// Exposes the CPU's M-profile architecture version. This isn't available in
/// rustc's standard environment.
///
/// This will set either `cfg(armv7m)` or `cfg(armv8m)` depending on the value
/// of the `TARGET` environment variable.
pub fn expose_m_profile() {
    let target = env::var("TARGET").unwrap();

    if target.starts_with("thumbv7m") || target.starts_with("thumbv7em") {
        println!("cargo:rustc-cfg=armv7m");
    } else if target.starts_with("thumbv8m") {
        println!("cargo:rustc-cfg=armv8m");
    } else {
        println!("Don't know the target {}", target);
        std::process::exit(1);
    }
}

/// Exposes the board type from the `HUBRIS_BOARD` envvar into
/// `cfg(target_board="...")`.
pub fn expose_target_board() {
    if let Ok(board) = env::var("HUBRIS_BOARD") {
        println!("cargo:rustc-cfg=target_board=\"{}\"", board);
    }
    println!("cargo:rerun-if-env-changed=HUBRIS_BOARD");
}

#[derive(Clone, Debug, Deserialize)]
pub struct I2cPin {
    pub port: Option<String>,
    pub pins: Vec<u8>,
    pub af: u8
}

#[derive(Clone, Debug, Deserialize)]
pub struct I2cMux {
    pub driver: String,
    pub address: u8,
    pub enable: Option<I2cPin>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct I2cPort {
    pub pins: Vec<I2cPin>,
    pub muxes: Option<Vec<I2cMux>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct I2cController {
    pub controller: u8,
    pub ports: IndexMap<String, I2cPort>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct I2cDevice {
    pub driver: String,
    pub controller: u8,
    pub address: u8,
}

#[derive(Clone, Debug, Deserialize)]
pub struct I2cConfig {
    pub controllers: Vec<I2cController>,
    pub devices: Vec<I2cDevice>
}

#[derive(Clone, Debug, Deserialize)]
struct Config {
    i2c: I2cConfig
}

pub fn i2c_config() -> I2cConfig {
    if let Ok(tree) = env::var("HUBRIS_APP_CONFIG") {
        let toml: Config = toml::from_slice(tree.as_bytes()).unwrap();
        toml.i2c
    } else {
        I2cConfig {
            controllers: vec![],
            devices: vec![],
        }
    }
}
