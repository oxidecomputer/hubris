// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Result, bail};
use build_i2c::{CodegenSettings, Disposition, I2cConfig, Report};
use std::{fs::File, io::Write, path::Path};

use crate::config::Config;

/// Which stage of the I2C pipeline to run (and dump the result of).
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Stage {
    /// Load the manifest and dump the parsed I2C configuration.
    Load,

    /// Load and analyze the manifest, and dump the resulting report.
    Analysis,

    /// Load, analyze, and generate code (the default).
    Codegen,
}

/// Load the I2C section of an application manifest.
pub fn load_config(cfg: &Path) -> Result<I2cConfig> {
    let cfg = Config::from_file(cfg)?;

    // This is a little roundabout of a process, but roughly approximates what
    // we do in normal builds where `xtask dist` will prepare the app toml and
    // shove it in an env var, and then i2c codegen will pull it from there.
    //
    // We convert to a string using *xtask*'s notion of manifest tomls...
    let config = toml::to_string(&cfg.config)?;

    // ...and now that it's a string, parse the contents back as *i2c*'s
    // different notion of what a manifest toml looks like (mostly just the
    // i2c config section).
    build_i2c::load::parse(&config)
}

/// Load and analyze the I2C section of an application manifest.
pub fn setup_report(cfg: &Path, settings: &CodegenSettings) -> Result<Report> {
    settings.analyze(load_config(cfg)?)
}

pub fn write_file(code: &str, output: &Path, fmt: bool) -> Result<()> {
    let mut f = File::create(output)?;
    f.write_all(code.as_bytes())?;
    f.flush()?;
    drop(f);
    if fmt {
        call_rustfmt::rustfmt_with_config(
            output,
            Some(Path::new(".rustfmt.toml")),
        )?;
    }
    Ok(())
}

/// Runs the I2C pipeline up to `stage` and returns a textual dump of that
/// stage's result: the parsed configuration, the analysis report, or the
/// generated code.
pub fn run_stage(
    cfg: &Path,
    disp: Disposition,
    stage: Stage,
) -> Result<String> {
    let config = load_config(cfg)?;
    if stage == Stage::Load {
        return Ok(format!("{config:#?}"));
    }

    let settings: CodegenSettings = disp.into();
    let report = settings.analyze(config)?;
    if stage == Stage::Analysis {
        return Ok(format!("{report:#?}"));
    }

    let build_i2c::CodegenOutputs { code, .. } =
        build_i2c::codegen(report, &settings)?;
    Ok(code)
}

/// Do I2C code generation (or dump an earlier stage of the pipeline).
pub fn run(
    cfg: &Path,
    disp: Disposition,
    stage: Stage,
    output: Option<&Path>,
    fmt: bool,
) -> Result<()> {
    if fmt && stage != Stage::Codegen {
        bail!("--fmt only applies to the codegen stage");
    }

    let text = run_stage(cfg, disp, stage)?;

    if let Some(p) = output {
        write_file(&text, p, fmt)?;
    } else {
        println!("{text}");
    }

    Ok(())
}
