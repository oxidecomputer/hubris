// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tests for stage 3 of the pipeline: code generation.
//!
//! These are snapshots of individual sections, generated from tiny manifests.

use build_i2c::analysis::{AnalysisSettings, ControllerRole};
use build_i2c::{Codegen, CodegenTarget, analysis, load};
use insta::assert_snapshot;
use std::collections::HashSet;

const CONTROLLERS: &str = r#"
[i2c]

[[i2c.controllers]]
controller = 2

[i2c.controllers.ports.B]
name = "bus1"
scl = { pin = 10 }
sda = { pin = 11 }
af = 4

[i2c.controllers.ports.F]
name = "bus2"
scl = { gpio_port = "H", pin = 12 }
sda = { gpio_port = "H", pin = 13 }
af = 4

[[i2c.controllers]]
controller = 3

[i2c.controllers.ports.A]
name = "solo"
scl = { pin = 1 }
sda = { pin = 2 }
af = 4
muxes = [
    { driver = "pca9548", address = 0x70 },
    { driver = "ltc4306", address = 0x44, nreset = { port = "G", pin = 5 } },
]
"#;

struct Fixture {
    report: analysis::Report,
}

impl Fixture {
    fn new(toml: &str) -> Self {
        Self::with(toml, ControllerRole::Initiator, false, None)
    }

    fn with(
        toml: &str,
        role: ControllerRole,
        component_ids: bool,
        drivers: Option<&[&str]>,
    ) -> Self {
        let settings = AnalysisSettings {
            role,
            component_ids,
            drivers: drivers.map(|d| {
                d.iter().map(|d| d.to_string()).collect::<HashSet<_>>()
            }),
        };

        let config = load::parse(toml).unwrap();
        let report = analysis::analyze(config, &settings).unwrap();

        Fixture { report }
    }

    fn codegen(&self, target: CodegenTarget) -> Codegen<'_> {
        Codegen {
            report: &self.report,
            codegen_target: target,
        }
    }

    fn section<'a>(
        &'a self,
        f: impl Fn(&Codegen<'a>, &mut String) -> anyhow::Result<()>,
    ) -> String {
        self.section_for(CodegenTarget::Stm32H753, f)
    }

    fn section_for<'a>(
        &'a self,
        target: CodegenTarget,
        f: impl Fn(&Codegen<'a>, &mut String) -> anyhow::Result<()>,
    ) -> String {
        let mut out = String::new();
        f(&self.codegen(target), &mut out).unwrap();
        out
    }
}

fn devices(extra: &str) -> String {
    format!("{CONTROLLERS}{extra}")
}

#[test]
fn controllers() {
    let f = Fixture::new(CONTROLLERS);
    assert_snapshot!(f.section(Codegen::generate_controllers));
}

#[test]
fn controllers_empty() {
    // No controller is configured as a target, so the list is empty.
    let f = Fixture::with(CONTROLLERS, ControllerRole::Target, false, None);
    assert_snapshot!(f.section(Codegen::generate_controllers));
}

#[test]
fn pins() {
    let f = Fixture::new(CONTROLLERS);
    assert_snapshot!(f.section(Codegen::generate_pins));
}

#[test]
fn ports() {
    let f = Fixture::new(CONTROLLERS);
    assert_snapshot!(f.section(Codegen::generate_ports));
}

#[test]
fn muxes() {
    // Controller 3 has two muxes: one with an nreset, one without.
    let f = Fixture::new(CONTROLLERS);
    assert_snapshot!(f.section(Codegen::generate_muxes));
}

#[test]
fn muxes_empty() {
    let f = Fixture::new(
        r#"
[i2c]

[[i2c.controllers]]
controller = 2

[i2c.controllers.ports.B]
scl = { pin = 10 }
sda = { pin = 11 }
af = 4
"#,
    );
    assert_snapshot!(f.section(Codegen::generate_muxes));
}

#[test]
fn device_with_mux_and_name() {
    let f = Fixture::new(&devices(
        r#"
[[i2c.devices]]
device = "tmp117"
name = "north"
refdes = ["J1", "U7"]
controller = 3
mux = 2
segment = 3
address = 0x48
description = "a muxed temperature sensor"
"#,
    ));
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn device_with_component_ids() {
    let toml = devices(
        r#"
[[i2c.devices]]
device = "tmp117"
refdes = "U1"
bus = "bus1"
address = 0x48
description = "a temperature sensor"
"#,
    );

    let f = Fixture::with(&toml, ControllerRole::Initiator, true, None);
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn pmbus_with_phases() {
    let f = Fixture::new(&devices(
        r#"
[[i2c.devices]]
device = "raa229618"
bus = "bus1"
address = 0x35
description = "a power controller"
power = { rails = ["V1P8", "VDD"], phases = [[0, 1], [2, 3]] }
"#,
    ));
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn pmbus_without_phases() {
    let f = Fixture::new(&devices(
        r#"
[[i2c.devices]]
device = "raa229618"
bus = "bus1"
address = 0x35
description = "a power controller"
power = { rails = ["V1P8"] }
"#,
    ));
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn non_pmbus_power() {
    // A non-PMBus device's rails show up in both the `pmbus` and the `power`
    // module.
    let f = Fixture::new(&devices(
        r#"
[[i2c.devices]]
device = "lm5066i"
bus = "bus1"
address = 0x16
description = "a hot swap controller"
power = { rails = ["V54"], pmbus = false }
"#,
    ));
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn sensors_single_and_array_fields() {
    let f = Fixture::new(&devices(
        r#"
[[i2c.devices]]
device = "tmp117"
name = "north"
bus = "bus1"
address = 0x48
description = "a single temperature sensor"
sensors = { temperature = 1 }

[[i2c.devices]]
device = "max31790"
refdes = "U42"
bus = "bus2"
address = 0x20
description = "a fan controller"
sensors = { speed = 3 }
"#,
    ));
    assert_snapshot!(f.section(Codegen::generate_sensors));
}

#[test]
fn sensors_without_sensors() {
    let f = Fixture::new(&devices(
        r#"
[[i2c.devices]]
device = "tmp117"
name = "north"
bus = "bus1"
address = 0x48
description = "a sensorless device"
"#,
    ));
    assert_snapshot!(f.section(Codegen::generate_sensors));
}

#[test]
fn validation_driver_and_raw_read() {
    let toml = devices(
        r#"
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "a device with a driver"

[[i2c.devices]]
device = "nonesuch"
bus = "bus2"
address = 0x20
description = "a device without a driver"
validate-with-raw-read = true
"#,
    );

    let f = Fixture::with(
        &toml,
        ControllerRole::Initiator,
        false,
        Some(&["tmp117"]),
    );
    assert_snapshot!(f.section(Codegen::generate_validation));
}

#[test]
fn match_arm_ranges_are_coalesced() {
    //
    // Devices 0, 1, 2 and 5 are on controller 2, while 3 and 4 are on
    // controller 3: the generated match arms should coalesce the former into
    // `0..=2 | 5..=5`.
    //
    let mut toml = String::from(CONTROLLERS);

    for (i, bus) in ["bus1", "bus1", "bus1", "solo", "solo", "bus1"]
        .iter()
        .enumerate()
    {
        toml.push_str(&format!(
            r#"
[[i2c.devices]]
device = "tmp117"
bus = "{bus}"
address = {address}
description = "device {i}"
"#,
            address = 0x48 + i,
        ));
    }

    let f = Fixture::new(&toml);
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn header_and_footer() {
    let f = Fixture::new(CONTROLLERS);
    let mut out = f.section(Codegen::generate_header);
    out.push_str(&f.section(Codegen::generate_footer));
    assert_snapshot!(out);
}

#[test]
fn controllers_for_each_target() {
    // The chip selection only affects the `use` statement in `controllers()`.
    let f = Fixture::new(CONTROLLERS);

    for target in [
        CodegenTarget::None,
        CodegenTarget::Stm32H743,
        CodegenTarget::Stm32G031,
        CodegenTarget::Stm32G030,
    ] {
        let out = f.section_for(target, Codegen::generate_controllers);
        let line = out
            .lines()
            .find(|l| l.contains("as device;"))
            .unwrap_or("(none)");
        insta::assert_snapshot!(format!("{target:?}"), line);
    }
}
