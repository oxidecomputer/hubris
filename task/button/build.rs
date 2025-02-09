// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
use build_lpc55pins::PinConfig;
use idol::{server::ServerStyle, CounterSettings};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct TaskConfig {
    pins: Vec<PinConfig>,
}

fn main() -> Result<()> {
    build_util::expose_target_board();
    build_util::build_notifications()?;

    idol::Generator::new()
        .with_counters(CounterSettings::default().with_server_counters(false))
        .build_server_support(
            "../../idl/button.idol",
            "server_stub.rs",
            ServerStyle::InOrder,
        )
        .unwrap();

    let task_config = build_util::task_config::<TaskConfig>()?;
    build_lpc55pins::codegen(task_config.pins)?;

    Ok(())
}
