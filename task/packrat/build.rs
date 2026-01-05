// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use anyhow::{anyhow, Result};

#[cfg(feature = "ereport")]
use anyhow::Context;

fn main() -> Result<()> {
    build_util::build_notifications()?;

    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/packrat.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )
        .map_err(|e| anyhow!("{e}"))?;

    // Ensure the "gimlet" feature is enabled on gimlet boards.
    #[cfg(not(feature = "gimlet"))]
    if let Some(
        "gimlet-b" | "gimlet-c" | "gimlet-d" | "gimlet-e" | "gimlet-f",
    ) = build_util::target_board().as_deref()
    {
        panic!(concat!(
            "packrat's `gimlet` feature should be enabled when ",
            "building for gimlets",
        ))
    }

    // Ensure the "gimlet" feature is _not_ enabled on sidecar/psc boards.
    #[cfg(feature = "gimlet")]
    match build_util::target_board().as_deref() {
        Some("psc-a" | "psc-b" | "psc-c") => panic!(concat!(
            "packrat's `gimlet` feature should not be enabled when ",
            "building for PSCs",
        )),
        Some("sidecar-b" | "sidecar-c" | "sidecar-d") => panic!(concat!(
            "packrat's `gimlet` feature should not be enabled when ",
            "building for sidecars",
        )),
        _ => (),
    }

    #[cfg(feature = "ereport")]
    gen_ereport_config().context("failed to generate ereport config")?;

    Ok(())
}

#[cfg(feature = "ereport")]
fn gen_ereport_config() -> Result<()> {
    use std::io::Write;

    let our_name = build_util::task_name();
    let tasks = build_util::task_ids();
    let id = tasks.get(&our_name).ok_or_else(|| {
        anyhow!(
            "task ID for {our_name:?} not found in task IDs map; this is \
             probably a bug in the build system",
        )
    })?;
    let id = u16::try_from(id).with_context(|| {
        format!(
            "packrat's task ID ({id}) exceeds u16::MAX, this is definitely \
             a bug"
        )
    })?;

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("ereport_config.rs");

    let mut out = std::fs::File::create(&dest_path).with_context(|| {
        format!("failed to create file {}", dest_path.display())
    })?;
    writeln!(
        out,
        "{}",
        quote::quote! {
            pub(crate) const TASK_ID: u16 = #id;
        }
    )
    .with_context(|| format!("failed to write to {}", dest_path.display()))?;

    Ok(())
}
