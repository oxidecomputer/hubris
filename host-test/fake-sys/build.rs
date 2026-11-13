// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::build_notifications()?;

    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/stm32xx-sys.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )?;

    // robo: The EXTI dispatch table comes from the same `gpio-irqs` config,
    // robo: and the same generator, as the real sys task's.
    let cfg = build_stm32xx_sys::SysConfig::load()?;
    let out = build_util::out_dir().join("exti_config.rs");
    let mut out = std::fs::File::create(out)?;
    writeln!(out, "{}", cfg.generate_exti_config()?)?;

    Ok(())
}
