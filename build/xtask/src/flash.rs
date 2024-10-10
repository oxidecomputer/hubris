// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize, Default)]
pub struct FlashConfig {
    /// The name used by probe-rs to identify the chip.
    chip: Option<String>,
}

impl FlashConfig {
    //
    // Set the chip
    //
    fn set_chip(&mut self, val: &str) -> &mut Self {
        self.chip = Some(val.to_string());
        self
    }
}

pub fn config(
    board: &str,
    _chip_dir: &Path,
) -> anyhow::Result<Option<FlashConfig>> {
    let mut flash = FlashConfig::default();

    flash.set_chip(chip_name(board)?);

    Ok(Some(flash))
}

pub fn chip_name(board: &str) -> anyhow::Result<&'static str> {
    let b = match board {
        "lpcxpresso55s69"
        | "rot-carrier-2"
        | "oxide-rot-1"
        | "oxide-rot-1-selfsigned" => "LPC55S69JBD100",
        "rot-carrier-1" => "LPC55S28JBD100",
        "stm32f3-discovery" => "STM32F303VCTx",
        "stm32f4-discovery" => "STM32F407VGTx",
        "nucleo-h743zi2" => "STM32H743ZITx",
        "nucleo-h753zi" => "STM32H753ZITx",
        "gemini-bu-1" | "gimletlet-1" | "gimletlet-2" | "gimlet-b"
        | "gimlet-c" | "gimlet-d" | "gimlet-e" | "gimlet-f" | "psc-a"
        | "psc-b" | "psc-c" | "sidecar-b" | "sidecar-c" | "sidecar-d"
        | "medusa-a" | "grapefruit" => "STM32H753ZITx",
        "donglet-g030" => "STM32G030F6Px",
        "donglet-g031" => "STM32G031F8Px",
        "stm32g031-nucleo" => "STM32G031Y8Yx",
        "oxcon2023g0" => "STM32G030J6Mx",
        "stm32g070-nucleo" => "STM32G070KBTx",
        "stm32g0b1-nucleo" => anyhow::bail!(
            "This board is not yet supported by probe-rs, \
            please use OpenOCD directly"
        ),
        _ => anyhow::bail!("unrecognized board {}", board),
    };

    Ok(b)
}
