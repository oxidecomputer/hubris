// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tests for stage 3 of the pipeline: code generation.
//!
//! These are snapshots of individual sections, generated from hand-built
//! [`Report`]s: each test supplies only the parts of a `Report` that the
//! section under test reads, as plain Rust values, so codegen is tested in
//! isolation from analysis.

mod common;

use build_i2c::analysis::{
    ControllerPort, Device, DeviceBus, DeviceName, DeviceRefdes, DeviceSensor,
    MuxSegment, NamedPort, OtherSensors, PowerRail, PowerRails, Report,
    SensorStruct, Validation,
};
use build_i2c::load::{
    I2cController, I2cDevice, I2cPower, I2cSensors, OtherSensorDevice, Refdes,
    Sensor,
};
use build_i2c::{Codegen, CodegenTarget, codegen};
use insta::assert_snapshot;
use proc_macro2::TokenStream;
use std::collections::BTreeMap;

/// Renders a token stream as formatted source, for readable snapshots.
fn pretty(tokens: TokenStream) -> String {
    let file = syn::parse2(tokens).expect("generated code should parse");
    prettyplease::unparse(&file)
}

//
// Two controllers: controller 2 has two ports (and so requires devices to
// name one), while controller 3 has a single port (with two muxes on it).
//
fn standard_controllers() -> Vec<I2cController> {
    vec![
        common::controller(
            2,
            [
                (
                    "B",
                    common::port(
                        Some("bus1"),
                        common::pin(None, 10),
                        common::pin(None, 11),
                        4,
                        vec![],
                    ),
                ),
                (
                    "F",
                    common::port(
                        Some("bus2"),
                        common::pin(Some("H"), 12),
                        common::pin(Some("H"), 13),
                        4,
                        vec![],
                    ),
                ),
            ],
        ),
        common::controller(
            3,
            [(
                "A",
                common::port(
                    Some("solo"),
                    common::pin(None, 1),
                    common::pin(None, 2),
                    4,
                    vec![
                        common::mux("pca9548", 0x70, None),
                        common::mux(
                            "ltc4306",
                            0x44,
                            Some(common::gpio("G", 5)),
                        ),
                    ],
                ),
            )],
        ),
    ]
}

/// The ports of `standard_controllers()`, as `analyze` would have resolved them.
fn named_ports() -> Vec<NamedPort> {
    vec![
        NamedPort {
            name: "B".into(),
            port: ControllerPort {
                controller: 2,
                index: 0,
            },
        },
        NamedPort {
            name: "F".into(),
            port: ControllerPort {
                controller: 2,
                index: 1,
            },
        },
        NamedPort {
            name: "A".into(),
            port: ControllerPort {
                controller: 3,
                index: 0,
            },
        },
    ]
}

struct Fixture {
    report: Report,
}

impl Fixture {
    fn section<'a>(
        &'a self,
        f: impl Fn(&Codegen<'a>) -> anyhow::Result<TokenStream>,
    ) -> String {
        self.section_for(CodegenTarget::Stm32H753, f)
    }

    fn section_for<'a>(
        &'a self,
        target: CodegenTarget,
        f: impl Fn(&Codegen<'a>) -> anyhow::Result<TokenStream>,
    ) -> String {
        pretty(
            f(&Codegen {
                report: &self.report,
                codegen_target: target,
            })
            .unwrap(),
        )
    }
}

#[test]
fn controllers() {
    let report = Report {
        controllers: standard_controllers(),
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_controllers));
}

#[test]
fn controllers_empty() {
    let report = Report {
        controllers: vec![],
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_controllers));
}

#[test]
fn pins() {
    let report = Report {
        controllers: standard_controllers(),
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_pins));
}

#[test]
fn ports() {
    let report = Report {
        ports: named_ports(),
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_ports));
}

#[test]
fn muxes() {
    // Controller 3 has two muxes: one with an nreset, one without.
    let report = Report {
        controllers: standard_controllers(),
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_muxes));
}

#[test]
fn muxes_empty() {
    let report = Report {
        controllers: vec![common::controller(
            2,
            [(
                "B",
                common::port(
                    None,
                    common::pin(None, 10),
                    common::pin(None, 11),
                    4,
                    vec![],
                ),
            )],
        )],
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_muxes));
}

#[test]
fn device_with_mux_and_name() {
    let config = I2cDevice {
        name: Some("north".into()),
        refdes: Some(Refdes::Path(vec!["J1".into(), "U7".into()])),
        controller: Some(3),
        mux: Some(2),
        segment: Some(3),
        ..common::device("tmp117", 0x48, "a muxed temperature sensor")
    };
    let device = Device {
        segment: Some(MuxSegment { mux: 2, segment: 3 }),
        ..common::resolved(config, 3, 0)
    };

    let report = Report {
        devices: vec![device],
        by_device: vec![common::group("tmp117".to_string(), &[0])],
        by_name: vec![common::lookup(
            DeviceName {
                device: "tmp117".into(),
                name: "north".into(),
            },
            0,
        )],
        by_refdes: vec![common::lookup(
            DeviceRefdes {
                device: "tmp117".into(),
                refdes: Refdes::Path(vec!["J1".into(), "U7".into()]),
            },
            0,
        )],
        by_controller: vec![common::group(3u8, &[0])],
        by_port: vec![common::group(0usize, &[0])],
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn device_with_component_ids() {
    let config = I2cDevice {
        refdes: Some(Refdes::Component("U1".into())),
        bus: Some("bus1".into()),
        ..common::device("tmp117", 0x48, "a temperature sensor")
    };
    let device = Device {
        component_id: Some("U1".into()),
        ..common::resolved(config, 2, 0)
    };

    let report = Report {
        component_ids: true,
        max_component_id_len: 2,
        devices: vec![device],
        by_device: vec![common::group("tmp117".to_string(), &[0])],
        by_bus: vec![common::group(
            DeviceBus {
                device: "tmp117".into(),
                bus: "bus1".into(),
            },
            &[0],
        )],
        by_refdes: vec![common::lookup(
            DeviceRefdes {
                device: "tmp117".into(),
                refdes: Refdes::Component("U1".into()),
            },
            0,
        )],
        by_controller: vec![common::group(2u8, &[0])],
        by_port: vec![common::group(0usize, &[0])],
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn pmbus_with_phases() {
    let config = I2cDevice {
        bus: Some("bus1".into()),
        power: Some(I2cPower {
            rails: Some(vec!["V1P8".into(), "VDD".into()]),
            phases: Some(vec![vec![0, 1], vec![2, 3]]),
            ..Default::default()
        }),
        ..common::device("raa229618", 0x35, "a power controller")
    };

    let report = Report {
        devices: vec![common::resolved(config, 2, 0)],
        by_device: vec![common::group("raa229618".to_string(), &[0])],
        by_bus: vec![common::group(
            DeviceBus {
                device: "raa229618".into(),
                bus: "bus1".into(),
            },
            &[0],
        )],
        by_controller: vec![common::group(2u8, &[0])],
        by_port: vec![common::group(0usize, &[0])],
        rails: PowerRails {
            pmbus: vec![
                PowerRail {
                    rail: "V1P8".into(),
                    device: 0,
                    bank: Some(0),
                    phases: Some(vec![0, 1]),
                },
                PowerRail {
                    rail: "VDD".into(),
                    device: 0,
                    bank: Some(1),
                    phases: Some(vec![2, 3]),
                },
            ],
            non_pmbus: vec![],
        },
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn pmbus_without_phases() {
    let config = I2cDevice {
        bus: Some("bus1".into()),
        power: Some(I2cPower {
            rails: Some(vec!["V1P8".into()]),
            ..Default::default()
        }),
        ..common::device("raa229618", 0x35, "a power controller")
    };

    let report = Report {
        devices: vec![common::resolved(config, 2, 0)],
        by_device: vec![common::group("raa229618".to_string(), &[0])],
        by_bus: vec![common::group(
            DeviceBus {
                device: "raa229618".into(),
                bus: "bus1".into(),
            },
            &[0],
        )],
        by_controller: vec![common::group(2u8, &[0])],
        by_port: vec![common::group(0usize, &[0])],
        rails: PowerRails {
            pmbus: vec![PowerRail {
                rail: "V1P8".into(),
                device: 0,
                bank: None,
                phases: None,
            }],
            non_pmbus: vec![],
        },
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn non_pmbus_power() {
    // A non-PMBus device's rails show up in both the `pmbus` and the `power`
    // module.
    let config = I2cDevice {
        bus: Some("bus1".into()),
        power: Some(I2cPower {
            rails: Some(vec!["V54".into()]),
            pmbus: false,
            ..Default::default()
        }),
        ..common::device("lm5066i", 0x16, "a hot swap controller")
    };

    let rail = PowerRail {
        rail: "V54".into(),
        device: 0,
        bank: None,
        phases: None,
    };

    let report = Report {
        devices: vec![common::resolved(config, 2, 0)],
        by_device: vec![common::group("lm5066i".to_string(), &[0])],
        by_bus: vec![common::group(
            DeviceBus {
                device: "lm5066i".into(),
                bus: "bus1".into(),
            },
            &[0],
        )],
        by_controller: vec![common::group(2u8, &[0])],
        by_port: vec![common::group(0usize, &[0])],
        rails: PowerRails {
            pmbus: vec![rail.clone()],
            non_pmbus: vec![rail],
        },
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn sensors_single_and_array_fields() {
    let d0 = I2cDevice {
        name: Some("north".into()),
        bus: Some("bus1".into()),
        sensors: Some(I2cSensors {
            temperature: 1,
            ..Default::default()
        }),
        ..common::device("tmp117", 0x48, "a single temperature sensor")
    };
    let d1 = I2cDevice {
        refdes: Some(Refdes::Component("U42".into())),
        bus: Some("bus2".into()),
        sensors: Some(I2cSensors {
            speed: 3,
            ..Default::default()
        }),
        ..common::device("max31790", 0x20, "a fan controller")
    };

    let entries = [
        (
            0,
            DeviceSensor {
                refdes: None,
                name: Some("north".into()),
                kind: Sensor::Temperature,
                id: 0,
            },
        ),
        (
            1,
            DeviceSensor {
                refdes: Some(Refdes::Component("U42".into())),
                name: None,
                kind: Sensor::Speed,
                id: 1,
            },
        ),
        (
            1,
            DeviceSensor {
                refdes: Some(Refdes::Component("U42".into())),
                name: None,
                kind: Sensor::Speed,
                id: 2,
            },
        ),
        (
            1,
            DeviceSensor {
                refdes: Some(Refdes::Component("U42".into())),
                name: None,
                kind: Sensor::Speed,
                id: 3,
            },
        ),
    ];

    let report = Report {
        devices: vec![common::resolved(d0, 2, 0), common::resolved(d1, 2, 1)],
        sensor_structs: vec![
            SensorStruct {
                name: "tmp117".into(),
                declare: true,
                labels: vec!["NORTH".into()],
            },
            SensorStruct {
                name: "max31790".into(),
                declare: true,
                labels: vec!["U42".into()],
            },
        ],
        sensors: common::sensors_description(
            &["tmp117", "max31790"],
            &entries,
            vec![],
        ),
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_sensors));
}

#[test]
fn sensors_without_sensors() {
    let config = I2cDevice {
        name: Some("north".into()),
        bus: Some("bus1".into()),
        ..common::device("tmp117", 0x48, "a sensorless device")
    };

    let report = Report {
        devices: vec![common::resolved(config, 2, 0)],
        sensor_structs: vec![SensorStruct {
            name: "tmp117".into(),
            declare: true,
            labels: vec!["NORTH".into()],
        }],
        sensors: common::sensors_description(&["tmp117"], &[], vec![]),
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_sensors));
}

#[test]
fn validation_driver_and_raw_read() {
    let d0 = I2cDevice {
        bus: Some("bus1".into()),
        ..common::device("tmp117", 0x48, "a device with a driver")
    };
    let d1 = I2cDevice {
        bus: Some("bus2".into()),
        validate_with_raw_read: true,
        ..common::device("nonesuch", 0x20, "a device without a driver")
    };

    let report = Report {
        devices: vec![common::resolved(d0, 2, 0), common::resolved(d1, 2, 1)],
        validation: Some(vec![
            Validation::Driver("Tmp117".into()),
            Validation::RawRead,
        ]),
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_validation));
}

#[test]
fn match_arm_ranges_are_coalesced() {
    //
    // Devices 0, 1, 2 and 5 are on controller 2, while 3 and 4 are on
    // controller 3: the generated match arms should coalesce the former into
    // `0..=2 | 5..=5`.
    //
    let buses = ["bus1", "bus1", "bus1", "solo", "solo", "bus1"];
    let locations = [(2u8, 0usize), (2, 0), (2, 0), (3, 0), (3, 0), (2, 0)];

    let devices: Vec<Device> = buses
        .iter()
        .enumerate()
        .map(|(i, bus)| {
            let config = I2cDevice {
                bus: Some((*bus).to_string()),
                ..common::device(
                    "tmp117",
                    0x48 + i as u8,
                    &format!("device {i}"),
                )
            };
            let (controller, index) = locations[i];
            common::resolved(config, controller, index)
        })
        .collect();

    let report = Report {
        devices,
        by_device: vec![common::group(
            "tmp117".to_string(),
            &[0, 1, 2, 3, 4, 5],
        )],
        by_bus: vec![
            common::group(
                DeviceBus {
                    device: "tmp117".into(),
                    bus: "bus1".into(),
                },
                &[0, 1, 2, 5],
            ),
            common::group(
                DeviceBus {
                    device: "tmp117".into(),
                    bus: "solo".into(),
                },
                &[3, 4],
            ),
        ],
        by_controller: vec![
            common::group(2u8, &[0, 1, 2, 5]),
            common::group(3u8, &[3, 4]),
        ],
        by_port: vec![common::group(0usize, &[0, 1, 2, 3, 4, 5])],
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_devices));
}

#[test]
fn module_wrapper() {
    let report = Report {
        ports: named_ports(),
        ..Default::default()
    };
    let f = Fixture { report };
    let ports = Codegen {
        report: &f.report,
        codegen_target: CodegenTarget::None,
    }
    .generate_ports()
    .unwrap();
    assert_snapshot!(pretty(codegen::i2c_config_module(ports)));
}

#[test]
fn controllers_for_each_target() {
    // The chip selection only affects the `use` statement in `controllers()`.
    let report = Report {
        controllers: standard_controllers(),
        ..Default::default()
    };
    let f = Fixture { report };

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

#[test]
fn other_sensors() {
    let other_sensors = vec![
        OtherSensors {
            config: OtherSensorDevice {
                name: "dimm_a".into(),
                device: "ts0".into(),
                description: "DIMM A".into(),
                sensors: BTreeMap::from([(Sensor::Temperature, 1)]),
                refdes: Some(Refdes::Path(vec!["J1".into(), "U2".into()])),
            },
            ids_by_kind: BTreeMap::from([(Sensor::Temperature, vec![1])]),
        },
        OtherSensors {
            config: OtherSensorDevice {
                name: "fans".into(),
                device: "fpga".into(),
                description: "fan hub".into(),
                sensors: BTreeMap::from([(Sensor::Speed, 3)]),
                refdes: Some(Refdes::Component("U9".into())),
            },
            ids_by_kind: BTreeMap::from([(Sensor::Speed, vec![2, 3, 4])]),
        },
    ];

    let report = Report {
        sensors: common::sensors_description(&[], &[], other_sensors),
        ..Default::default()
    };
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_other_sensors));
}

#[test]
fn other_sensors_empty() {
    let report = Report::default();
    let f = Fixture { report };
    assert_snapshot!(f.section(Codegen::generate_other_sensors));
}

#[test]
fn sensor_lookup_tables() {
    let entries = [(
        0,
        DeviceSensor {
            refdes: Some(Refdes::Component("U7".into())),
            name: Some("north".into()),
            kind: Sensor::Temperature,
            id: 0,
        },
    )];

    let other_sensors = vec![
        OtherSensors {
            config: OtherSensorDevice {
                name: "dimm_a".into(),
                device: "ts0".into(),
                description: "DIMM A".into(),
                sensors: BTreeMap::from([(Sensor::Temperature, 1)]),
                refdes: Some(Refdes::Path(vec!["J1".into(), "U2".into()])),
            },
            ids_by_kind: BTreeMap::from([(Sensor::Temperature, vec![1])]),
        },
        OtherSensors {
            config: OtherSensorDevice {
                name: "fans".into(),
                device: "fpga".into(),
                description: "fan hub".into(),
                sensors: BTreeMap::from([(Sensor::Speed, 3)]),
                refdes: Some(Refdes::Component("U9".into())),
            },
            ids_by_kind: BTreeMap::from([(Sensor::Speed, vec![2, 3, 4])]),
        },
    ];

    let report = Report {
        sensors: common::sensors_description(
            &["tmp117"],
            &entries,
            other_sensors,
        ),
        ..Default::default()
    };
    let f = Fixture { report };

    let mut out = f.section(Codegen::generate_sensor_id_to_component_id);
    out.push_str(&f.section(Codegen::generate_sensor_id_to_name));
    assert_snapshot!(out);
}

#[test]
fn sensor_lookup_tables_need_refdes_and_name() {
    // An I2C sensor with neither a name nor a refdes.
    let entries = [(
        0,
        DeviceSensor {
            refdes: None,
            name: None,
            kind: Sensor::Temperature,
            id: 0,
        },
    )];

    let report = Report {
        sensors: common::sensors_description(&["tmp117"], &entries, vec![]),
        ..Default::default()
    };
    let g = Codegen {
        report: &report,
        codegen_target: CodegenTarget::None,
    };

    let err = g.generate_sensor_id_to_component_id().unwrap_err();
    assert!(format!("{err:#}").contains("has no refdes"), "{err:#}");

    let err = g.generate_sensor_id_to_name().unwrap_err();
    assert!(format!("{err:#}").contains("has no name"), "{err:#}");
}
