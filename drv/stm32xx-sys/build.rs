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

    let cfg = build_stm32xx_sys::SysConfig::load()?;

    const EXTI_FEATURE: &str = "exti";

    if build_util::has_feature(EXTI_FEATURE) {
        let out_dir = build_util::out_dir();
        let dest_path = out_dir.join("exti_config.rs");

        let mut out = std::fs::File::create(dest_path)?;

        let generated = cfg.generate_exti_config()?;
        writeln!(out, "{generated}")?;
    } else if cfg.needs_exti() {
        return Err(format!(
            "the \"drv-stm32xx-sys/{EXTI_FEATURE}\" feature is required in order to \
            configure GPIO pin interrupts"
        ).into());
    }

    Ok(())
}
