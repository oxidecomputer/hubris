// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, Context, Result};
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
pub fn config<T: knuffel::DecodeChildren<knuffel::span::Span>>() -> Result<T> {
    kdl_from_env("HUBRIS_APP_CONFIG")?.ok_or_else(|| {
        anyhow!("app.kdl missing global config section [config]")
    })
}

pub fn config_key<T: knuffel::DecodeChildren<knuffel::span::Span>>(key: &str) -> Result<T> {
    sub_kdl_from_env("HUBRIS_APP_CONFIG", key)?.ok_or_else(|| {
        anyhow!("app.kdl missing global config section [config.{}]", key)
    })
}

/// Pulls the task configuration. See `config` for more details.
pub fn task_config<T: knuffel::DecodeChildren<knuffel::span::Span>>() -> Result<T> {
    let task_name =
        env::var("HUBRIS_TASK_NAME").expect("missing HUBRIS_TASK_NAME");
    task_maybe_config()?.ok_or_else(|| {
        anyhow!(
            "app.kdl missing task config section [tasks.{}.config]",
            task_name
        )
    })
}

/// Pulls the task configuration, or `None` if the configuration is not
/// provided.
pub fn task_maybe_config<T: knuffel::DecodeChildren<knuffel::span::Span>>() -> Result<Option<T>> {
    kdl_from_env("HUBRIS_TASK_CONFIG")
}

/// Parse the contents of an environment variable as KDL.
///
/// Returns:
///
/// - `Ok(Some(x))` if the environment variable is defined and the contents
///   deserialized correctly.
/// - `Ok(None)` if the environment variable is not defined.
/// - `Err(e)` if deserialization failed or the environment variable did not
///   contain UTF-8.
fn kdl_from_env<T: knuffel::DecodeChildren<knuffel::span::Span>>(var: &str) -> Result<Option<T>> {
    let config = match env::var(var) {
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| {
                format!("accessing environment variable {}", var)
            })
        }
        Ok(c) => c,
    };

    println!("--- kdl for ${} ---", var);
    println!("{}", config);
    let rval = match knuffel::parse(var, &config) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{:?}", miette::Report::new(e));
            anyhow::bail!("error parsing KDL");
        }
    };
    println!("cargo:rerun-if-env-changed={}", var);
    Ok(Some(rval))
}

fn sub_kdl_from_env<T: knuffel::DecodeChildren<knuffel::span::Span>>(var: &str, key: &str) -> Result<Option<T>> {
    let config = match env::var(var) {
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| {
                format!("accessing environment variable {}", var)
            })
        }
        Ok(c) => c,
    };

    println!("--- kdl for ${} ---", var);
    println!("{}", config);
    let doc: kdl::KdlDocument = config.parse()?;
    let node = doc.get(key).ok_or_else(|| anyhow!("config key {} missing", key))?;
    let rval = match knuffel::parse(var, &node.to_string()) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{:?}", miette::Report::new(e));
            anyhow::bail!("error parsing KDL");
        }
    };
    println!("cargo:rerun-if-env-changed={}", var);
    Ok(Some(rval))
}

