// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tests for stage 1 of the pipeline: loading a manifest.

use build_i2c::load::{self, I2cConfig, Refdes};

fn parse(toml: &str) -> I2cConfig {
    load::parse(toml).unwrap()
}

const ONE_CONTROLLER: &str = r#"
[i2c]

[[i2c.controllers]]
controller = 2

[i2c.controllers.ports.B]
name = "bus1"
description = "the one bus"
scl = { pin = 10 }
sda = { pin = 11 }
af = 4
"#;

#[test]
fn minimal_manifest() {
    let cfg = parse(&format!(
        r#"{ONE_CONTROLLER}
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "a temperature sensor"
"#
    ));

    assert_eq!(cfg.controllers.len(), 1);
    let c = &cfg.controllers[0];
    assert_eq!(c.controller, 2);
    assert_eq!(c.ports.len(), 1);

    let port = &c.ports["B"];
    assert_eq!(port.name.as_deref(), Some("bus1"));
    assert_eq!(port.scl.pin, 10);
    assert_eq!(port.scl.gpio_port, None);
    assert_eq!(port.sda.pin, 11);
    assert_eq!(port.af, 4);
    assert!(port.muxes.is_empty());

    let devices = cfg.devices.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device, "tmp117");
    assert_eq!(devices[0].bus.as_deref(), Some("bus1"));
    assert_eq!(devices[0].address, 0x48);
}

#[test]
fn defaults() {
    let cfg = parse(&format!(
        r#"{ONE_CONTROLLER}
[[i2c.devices]]
device = "raa229618"
bus = "bus1"
address = 0x35
description = "a power controller"
power = {{ rails = ["V1"] }}
sensors = {{ temperature = 1 }}
"#
    ));

    // `target` defaults to false...
    assert!(!cfg.controllers[0].target);

    let devices = cfg.devices.unwrap();
    let d = &devices[0];

    // ...as do `removable` and `validate-with-raw-read`...
    assert!(!d.removable);
    assert!(!d.validate_with_raw_read);

    // ...while `pmbus` defaults to true...
    assert!(d.power.as_ref().unwrap().pmbus);

    // ...and every sensor count defaults to zero.
    let sensors = d.sensors.as_ref().unwrap();
    assert_eq!(sensors.temperature, 1);
    assert_eq!(sensors.power, 0);
    assert_eq!(sensors.current, 0);
    assert_eq!(sensors.voltage, 0);
    assert_eq!(sensors.input_current, 0);
    assert_eq!(sensors.input_voltage, 0);
    assert_eq!(sensors.speed, 0);
    assert_eq!(sensors.names, None);
}

#[test]
fn enable_is_an_alias_for_nreset() {
    let by_nreset = parse(
        r#"
[i2c]

[[i2c.controllers]]
controller = 2

[i2c.controllers.ports.B]
scl = { pin = 10 }
sda = { pin = 11 }
af = 4
muxes = [{ driver = "pca9548", address = 0x70, nreset = { port = "A", pin = 3 } }]
"#,
    );

    let by_enable = parse(
        r#"
[i2c]

[[i2c.controllers]]
controller = 2

[i2c.controllers.ports.B]
scl = { pin = 10 }
sda = { pin = 11 }
af = 4
muxes = [{ driver = "pca9548", address = 0x70, enable = { port = "A", pin = 3 } }]
"#,
    );

    for cfg in [by_nreset, by_enable] {
        let mux = &cfg.controllers[0].ports["B"].muxes[0];
        let nreset = mux.nreset.as_ref().unwrap();
        assert_eq!(nreset.port, "A");
        assert_eq!(nreset.pin, 3);
    }
}

#[test]
fn refdes_is_untagged() {
    let cfg = parse(&format!(
        r#"{ONE_CONTROLLER}
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "a component"
refdes = "U42"

[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x49
description = "a path"
refdes = ["J1", "U7"]
"#
    ));

    let devices = cfg.devices.unwrap();
    assert_eq!(
        devices[0].refdes,
        Some(Refdes::Component("U42".to_string()))
    );
    assert_eq!(
        devices[1].refdes,
        Some(Refdes::Path(vec!["J1".to_string(), "U7".to_string()]))
    );

    // The component ID is the path, joined and upper-cased.
    assert_eq!(devices[0].refdes.as_ref().unwrap().to_component_id(), "U42");
    assert_eq!(
        devices[1].refdes.as_ref().unwrap().to_component_id(),
        "J1/U7"
    );
}

#[test]
fn ports_are_sorted() {
    //
    // The ordering of ports is load-bearing: it determines port indices, so
    // deserialization must impose its own ordering rather than preserving the
    // order in which ports appear in the manifest.
    //
    let cfg = parse(
        r#"
[i2c]

[[i2c.controllers]]
controller = 2

[i2c.controllers.ports.F]
scl = { pin = 1 }
sda = { pin = 2 }
af = 4

[i2c.controllers.ports.B]
scl = { pin = 3 }
sda = { pin = 4 }
af = 4

[i2c.controllers.ports.D]
scl = { pin = 5 }
sda = { pin = 6 }
af = 4
"#,
    );

    let names: Vec<_> = cfg.controllers[0].ports.keys().collect();
    assert_eq!(names, ["B", "D", "F"]);
}

#[test]
fn unknown_fields_are_rejected() {
    let err = load::parse(&format!(
        r#"{ONE_CONTROLLER}
[[i2c.devices]]
device = "tmp117"
bus = "bus1"
address = 0x48
description = "a temperature sensor"
gizmo = true
"#
    ))
    .unwrap_err();

    assert!(
        format!("{err:#}").contains("gizmo"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn unknown_i2c_fields_are_rejected() {
    let err =
        load::parse(&format!("{ONE_CONTROLLER}\nnonsense = 1\n")).unwrap_err();

    assert!(
        format!("{err:#}").contains("nonsense"),
        "unexpected error: {err:#}"
    );
}
