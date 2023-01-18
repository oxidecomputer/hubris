// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use anyhow::{anyhow, bail, Result};

fn main() -> Result<()> {
    idol::server::build_server_support(
        "../../idl/spi.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )
    .map_err(|e| anyhow!(e))?;

    // The SPI server task hard-codes the SPI IRQ to 1; we check the app.toml
    // here to make sure that it agrees.
    let task_config = build_util::task_full_config_toml()?;
    let re = regex::Regex::new(r"^spi\d\.irq$").unwrap();
    const EXPECTED_IRQ: u32 = 0b1;
    for (k, v) in &task_config.interrupts {
        if re.is_match(k) {
            if *v != EXPECTED_IRQ {
                bail!("{k} must be {EXPECTED_IRQ:#b}");
            }
        }
    }

    Ok(())
}
