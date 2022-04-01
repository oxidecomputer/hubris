// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, Result};
use serde::de::DeserializeOwned;
use std::env;

/// Exposes the CPU's M-profile architecture version. This isn't available in
/// rustc's standard environment.
///
/// This will set one of `cfg(armv6m`), `cfg(armv7m)`, or `cfg(armv8m)`
/// depending on the value of the `TARGET` environment variable.
pub fn expose_m_profile() {
    let target = env::var("TARGET").unwrap();

    if target.starts_with("thumbv6m") {
        println!("cargo:rustc-cfg=armv6m");
    } else if target.starts_with("thumbv7m") || target.starts_with("thumbv7em")
    {
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

///
/// Pulls the app-wide configuration for purposes of a build task.  This
/// will fail if the app-wide configuration doesn't exist or can't parse.
/// Note that -- thanks to the magic of Serde -- `T` need not (and indeed,
/// should not) contain the entire app-wide configuration, but rather only
/// those parts that a particular build task cares about.  (It should go
/// without saying that `deny_unknown_fields` should *not* be set on this
/// type -- but it may well be set within the task-specific types that
/// this type contains.)  If the configuration field is optional, `T` should
/// reflect that by having its member (or members) be an `Option` type.
///
pub fn config<T: DeserializeOwned>() -> Result<T> {
    toml_from_env("HUBRIS_APP_CONFIG", Some("[config]"))
}

/// Pulls the task configuration. See `config` for more details.
pub fn task_config<T: DeserializeOwned>() -> Result<T> {
    let section = match env::var("HUBRIS_TASK_NAME") {
        Ok(task_name) => Some(format!("[tasks.{}.config]", task_name)),
        _ => None,
    };
    toml_from_env("HUBRIS_TASK_CONFIG", section.as_deref())
}

/// Parse the contents of an environment variable as toml. `section_name_pattern` is a string
/// indicating what section of original toml file the variable should have come from to improve error reporting. `{task}` in the pattern is replaced with HUBRIS_TASK_NAME.
fn toml_from_env<T: DeserializeOwned>(
    var: &str,
    section_name: Option<&str>,
) -> Result<T> {
    let config = env::var(var).map_err(|err|
        match err {
            env::VarError::NotPresent =>
                match section_name {
                    Some(section_name) => anyhow!("{} environment variable is undefined, but it should contain toml. Are you missing the {} section in your app.toml?", var, section_name),
                    None => anyhow!("{} environment variable should contain toml, but it's missing, and we don't know why. Something has gone horribly wrong.", var)
                },
            _ => anyhow!(err)
        }
    )?;

    println!("--- toml for ${} ---", var);
    println!("{}", config);
    let rval = toml::from_slice(config.as_bytes())?;
    println!("cargo:rerun-if-env-changed={}", var);
    Ok(rval)
}
