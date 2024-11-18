// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() {
    //
    // Set link flags when building bins
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
    }
}
