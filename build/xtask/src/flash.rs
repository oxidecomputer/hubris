// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Context as _;
use serde::Serialize;
use std::path::Path;

use crate::config::BoardConfig;

#[derive(Debug, Serialize, Default)]
pub struct FlashConfig {
    /// The name used by probe-rs to identify the chip.
    chip: Option<String>,
}

pub fn config(board: &str) -> anyhow::Result<FlashConfig> {
    Ok(FlashConfig {
        chip: chip_name(board)?,
    })
}

pub fn chip_name(board: &str) -> anyhow::Result<Option<String>> {
    let board_config_path = Path::new("boards").join(format!("{board}.toml"));

    let board_config_text = std::fs::read_to_string(&board_config_path)
        .with_context(|| {
            format!(
                "can't access board config at: {}",
                board_config_path.display()
            )
        })?;

    let board_config: BoardConfig = toml::from_str(&board_config_text)
        .with_context(|| {
            format!(
                "can't parse board config at: {}",
                board_config_path.display()
            )
        })?;

    if let Some(probe_rs) = &board_config.probe_rs {
        Ok(Some(probe_rs.chip_name.clone()))
    } else {
        // tolerate the section missing for new chips, but we can't provide a
        // chip name in this case.
        Ok(None)
    }
}
