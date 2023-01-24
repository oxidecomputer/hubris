// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use build_lpc55pins::PinConfig;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct TaskConfig {
    pins: Vec<PinConfig>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::build_notifications()?;
    let task_config = build_util::task_config::<TaskConfig>()?;

    build_lpc55pins::codegen(task_config.pins)?;
    Ok(())
}
