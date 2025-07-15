// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Context;

// Make various assertions about the handoff region
fn main() -> anyhow::Result<()> {
    let kconfig: build_kconfig::KernelConfig =
        ron::de::from_str(&build_util::env_var("HUBRIS_KCONFIG")?)
            .context("parsing kconfig from HUBRIS_KCONFIG")?;
    assert!(
        kconfig
            .features
            .contains(&"measurement-handoff".to_string()),
        "missing measurement-handoff feature"
    );
    let dtcm_range = kconfig
        .extern_regions
        .get("dtcm")
        .expect("missing `dtcm` in `extern_regions`");
    assert_eq!(
        dtcm_range.start,
        measurement_token::ADDR as u32,
        "invalid address for token"
    );
    assert!(
        (dtcm_range.end - dtcm_range.start) as usize
            >= 4 * std::mem::size_of::<u32>(),
        "range is not large enough for handoff"
    );
    Ok(())
}
