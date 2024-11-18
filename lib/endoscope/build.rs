// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
// use std::io::Write;
// use anyhow::{bail, Context};
// use std::process::Command;

// cargo::rustc-link-arg=FLAG — Passes custom flags to a linker for benchmarks, binaries, cdylib crates, examples, and tests.
// cargo::rustc-link-arg-bin=BIN=FLAG — Passes custom flags to a linker for the binary BIN.
// cargo::rustc-link-arg-bins=FLAG — Passes custom flags to a linker for binaries.
// cargo::rustc-link-lib=LIB — Adds a library to link.
// cargo::rustc-link-search=[KIND=]PATH — Adds to the library search path.
// cargo::rustc-flags=FLAGS — Passes certain flags to the compiler.
// cargo::rustc-cfg=KEY[="VALUE"] — Enables compile-time cfg settings.
// cargo::rustc-check-cfg=CHECK_CFG – Register custom cfgs as expected for compile-time checking of configs.
// cargo::rustc-env=VAR=VALUE — Sets an environment variable.
// cargo::error=MESSAGE — Displays an error on the terminal.
// cargo::warning=MESSAGE — Displays a warning on the terminal.
// cargo::metadata=KEY=VALUE — Metadata, used by links scripts.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Set link flags when building bins

    // println!("cargo::rustc-link-arg-bin=endoscope.stm32h753=");

    /*
    if let Ok(features) = std::env::var("CARGO_FEATURE_SOC_") {
        println!("cargo::warning=found features: {features}");
    }
    */

    let mut soc = None;
    for (key, _value) in std::env::vars() {
        if key.starts_with("CARGO_FEATURE_SOC_") {
            let soc_name = key
                .strip_prefix("CARGO_FEATURE_SOC_")
                .unwrap()
                .to_lowercase();
            if soc.is_some() {
                println!(
                    "cargo::error=Multiple 'soc_*' features enabled {}, {}",
                    soc.as_ref().unwrap(),
                    soc_name
                );
            } else {
                let cwd = std::env::current_dir().unwrap().join("scripts");

                println!("cargo::rustc-link-arg=--verbose");
                println!("cargo::rustc-link-arg-bins=-T{}.x", &soc_name);
                println!("cargo::rustc-link-search={}", cwd.to_str().unwrap());
                soc = Some(soc_name);
            }
        }
        // println!("cargo::warning={key}: {_value}");
    }
    Ok(())
}
