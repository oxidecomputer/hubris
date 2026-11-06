// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // On a native (host) target this sets no cfgs, but does expose the
    // cfg names to avoid warnings.
    build_util::expose_m_profile()?;

    // Do an architecture check.
    if build_util::target_os() != "none"
        && build_util::env_var("HUBRIS_HOST_BUILD").is_err()
    {
        eprintln!("***********************************************");
        eprintln!("Hi!");
        eprintln!("You appear to be building this natively,");
        eprintln!("i.e. for your workstation, outside of xtask.");
        eprintln!("For host builds, use `cargo xtask host-build`");
        eprintln!("or `cargo xtask host-run`. Otherwise, please");
        eprintln!("specify --target=some-triple, e.g.");
        eprintln!("--target=thumbv7em-none-eabihf");
        eprintln!("***********************************************");
        panic!()
    }

    Ok(())
}
