// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Okay, so this is actually testing `build/i2c`, and the test for this should
//! probably live there, but right now the easiest way to invoke code generation
//! is by using the xtask implementation. We can't depend on xtask from
//! build-i2c because that would lead to circular dependencies.
//!
//! So, pragmatically, xtask is going to host the snapshot testing party for
//! build-i2c to avoid churning a lot of other things.
//!
//! ALSO, note that the existence of this test is intended to be a temporary
//! band-aid for a lack of any testing for `build-i2c`. The hope is to someday
//! test this more directly using unit tests of analysis and much more targeted
//! snapshot testing of generation behavior, which will allow for much smaller
//! fragments and less breakage on intentional changes.
//!
//! Apologies to those that need to update these snapshots until that day comes.
//! You will need to install `cargo-insta`, e.g. `cargo install cargo-insta`,
//! run the tests with `cargo insta test -p xtask`, and then review+bless any
//! new changes with `cargo insta review`, OR by using environment variables,
//! see: <https://insta.rs/docs/quickstart/#tests-without-insta>.

use std::path::Path;

use anyhow::Result;
use build_i2c::{Codegen, CodegenSettings, Disposition};
use insta::assert_snapshot;
use proc_macro2::TokenStream;
use tempfile::{TempDir, tempdir};

type GenFn = fn(&Codegen<'_>) -> Result<TokenStream>;

//
// Thin wrappers around the code generation methods: a method of
// `Codegen<'a>` can't be named as a function pointer that is generic over
// `'a`, but a free function can.
//
macro_rules! gen_fns {
    ($($name:ident),* $(,)?) => {
        $(
            fn $name(g: &Codegen<'_>) -> Result<TokenStream> {
                g.$name()
            }
        )*
    };
}

gen_fns!(
    generate_controllers,
    generate_devices,
    generate_muxes,
    generate_pins,
    generate_ports,
    generate_sensors,
    generate_validation,
);

#[test]
fn snapshot() {
    let manifests: &[&Path] = &[
        Path::new("app/gimlet/rev-f-dev.toml"),
        Path::new("app/cosmo/rev-b-dev.toml"),
        Path::new("app/sidecar/rev-d-dev.toml"),
        Path::new("app/observer/rev-a-dev.toml"),
        Path::new("app/psc/rev-c-dev.toml"),
        // Generating a Gimletlet image is interesting as Gimletlet is
        // representative of boards which have no PMBus devices.
        Path::new("app/gimletlet/app-meanwell.toml"),
    ];

    // Note that the disposition named here only selects the analysis settings
    // (controller role, component IDs, validation drivers); each entry emits
    // exactly one section of the generated code.
    let funcs: &[(&str, Disposition, GenFn)] = &[
        ("controllers", Disposition::Initiator, generate_controllers),
        ("devices", Disposition::Sensors, generate_devices),
        ("muxes", Disposition::Sensors, generate_muxes),
        ("pins", Disposition::Initiator, generate_pins),
        ("ports", Disposition::Sensors, generate_ports),
        ("validation", Disposition::Validation, generate_validation),
        ("controllers", Disposition::Target, generate_controllers),
        ("pins", Disposition::Target, generate_pins),
        ("ports", Disposition::Target, generate_ports),
    ];

    let all_dispositions = [
        Disposition::Initiator,
        Disposition::Target,
        Disposition::Devices,
        Disposition::Sensors,
        Disposition::Validation,
    ];

    // oh no, loading manifests doesn't work if we aren't at the base of the
    // repository.
    std::env::set_current_dir(Path::new("../../")).unwrap();

    // temporary directory so we can invoke rustfmt on the snapshots
    let tempdir = tempdir().unwrap();

    for manifest in manifests {
        for (case, disp, f) in funcs {
            let name = manifest.to_string_lossy().replace("/", "_");
            let name = format!("{name}.{case}-{disp:?}");
            snapshot_file(manifest, &tempdir, (*disp).into(), &name, *f);
        }

        // Device generation with component IDs enabled (normally selected by
        // the `component-id` cargo feature).
        {
            let name = manifest.to_string_lossy().replace("/", "_");
            let name = format!("{name}.devices-Sensors-component-ids");
            let mut settings: CodegenSettings = Disposition::Sensors.into();
            settings.component_ids = true;
            snapshot_file(
                manifest,
                &tempdir,
                settings,
                &name,
                generate_devices,
            );
        }

        // The full, assembled output of `codegen()` for every disposition.
        // Some combinations are errors (e.g. `Target` with no target
        // controller), in which case we snapshot the error text instead.
        for disp in all_dispositions {
            let name = manifest.to_string_lossy().replace("/", "_");
            let name = format!("{name}.codegen-{disp:?}");
            let settings: CodegenSettings = disp.into();
            let report =
                xtask::i2c_codegen::setup_report(manifest, &settings).unwrap();
            match build_i2c::codegen(report, &settings) {
                Ok(outputs) => {
                    let dest = format!("{name}.snap");
                    let temp_out = tempdir.path().join(Path::new(&dest));
                    xtask::i2c_codegen::write_file(
                        &outputs.code,
                        &temp_out,
                        true,
                    )
                    .unwrap();
                    let contents = std::fs::read_to_string(temp_out).unwrap();
                    assert_snapshot!(name, contents);
                }
                Err(e) => {
                    assert_snapshot!(name, format!("ERROR: {e:#}"));
                }
            }
        }

        // The device descriptions consumed by other build scripts.
        {
            let name = manifest.to_string_lossy().replace("/", "_");
            let name = format!("{name}.device-descriptions");
            let settings: CodegenSettings = Disposition::Validation.into();
            let report =
                xtask::i2c_codegen::setup_report(manifest, &settings).unwrap();
            let descs: Vec<_> = report.device_descriptions().collect();
            assert_snapshot!(name, format!("{descs:#?}"));
        }

        // Handle sensors separately because the analysis produces a
        // description in addition to the generated code.
        let disp = Disposition::Sensors;
        let case = "sensors";
        let name = manifest.to_string_lossy().replace("/", "_");
        let name = format!("{name}.{case}-{disp:?}");

        let settings: CodegenSettings = disp.into();
        snapshot_file(
            manifest,
            &tempdir,
            settings.clone(),
            &name,
            generate_sensors,
        );

        // Now snapshot the generated description
        let report =
            xtask::i2c_codegen::setup_report(manifest, &settings).unwrap();
        let name = format!("{name}-desc");
        assert_snapshot!(name, report.sensors.to_string());
    }
}

fn snapshot_file(
    manifest: &Path,
    tempdir: &TempDir,
    settings: CodegenSettings,
    name: &str,
    f: GenFn,
) {
    let dest = format!("{name}.snap");
    let temp_out = tempdir.path().join(Path::new(&dest));

    // Load and analyze the manifest...
    let report = xtask::i2c_codegen::setup_report(manifest, &settings).unwrap();
    let g = Codegen {
        report: &report,
        codegen_target: settings.codegen_target,
    };
    // Do code generation with the given function
    let out = (f)(&g).unwrap().to_string();
    // Write and format the file...
    xtask::i2c_codegen::write_file(&out, &temp_out, true).unwrap();

    // ...then read it back
    let contents = std::fs::read_to_string(temp_out).unwrap();
    assert_snapshot!(name, contents);
}
