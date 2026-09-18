// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tests for stage 2 of the pipeline: analyzing a loaded manifest.

mod common;

use anyhow::Result;
use build_i2c::analysis::{
    self, AnalysisSettings, ControllerRole, DeviceBus, MuxSegment, Report,
    VpdKind,
};
use build_i2c::load::{
    Config, EepromVpd, I2cConfig, I2cController, I2cDevice, I2cPower,
    I2cSensors, OtherSensorDevice, Refdes, Sensor, SensorConfig,
};
use std::collections::{BTreeMap, HashSet};

//
// Two controllers: controller 2 has two ports (and so requires devices to
// name one), while controller 3 has a single port (with a mux on it).
//
fn controllers() -> Vec<I2cController> {
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
                        common::pin(None, 12),
                        common::pin(None, 13),
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
                    vec![common::mux("pca9548", 0x70, None)],
                ),
            )],
        ),
    ]
}

fn settings(role: ControllerRole) -> AnalysisSettings {
    AnalysisSettings {
        role,
        ..Default::default()
    }
}

/// Analyzes a full manifest built from its parts.
fn analyze_manifest(
    controllers: Vec<I2cController>,
    devices: Option<Vec<I2cDevice>>,
    sensor: Option<SensorConfig>,
    settings: AnalysisSettings,
) -> Result<Report> {
    analysis::analyze(
        Config {
            i2c: I2cConfig {
                controllers,
                devices,
            },
            sensor,
        },
        &settings,
    )
}

/// Analyzes the standard controllers plus the given devices.
fn analyze_with(
    devices: Vec<I2cDevice>,
    settings: AnalysisSettings,
) -> Result<Report> {
    analyze_manifest(controllers(), Some(devices), None, settings)
}

/// Analyzes the standard controllers plus the given devices, as an
/// initiator.
fn analyze(devices: Vec<I2cDevice>) -> Result<Report> {
    analyze_with(devices, settings(ControllerRole::Initiator))
}

/// Analyzes the standard controllers plus the given devices and non-I2C
/// sensors, as an initiator.
fn analyze_with_sensor(
    devices: Vec<I2cDevice>,
    sensor: Option<SensorConfig>,
) -> Result<Report> {
    analyze_manifest(
        controllers(),
        Some(devices),
        sensor,
        settings(ControllerRole::Initiator),
    )
}

/// Asserts that analysis fails with an error containing `needle`.
#[track_caller]
fn assert_error(devices: Vec<I2cDevice>, needle: &str) {
    match analyze(devices) {
        Ok(_) => panic!("expected an error containing {needle:?}"),
        Err(e) => {
            let msg = format!("{e:#}");
            assert!(
                msg.contains(needle),
                "error {msg:?} does not contain {needle:?}"
            );
        }
    }
}

/// A minimal `tmp117` device, with everything but `device`, `address`, and
/// `description` left for the test to fill in.
fn base_device() -> I2cDevice {
    common::device("tmp117", 0x48, "a temperature sensor")
}

//
// Resolution
//

#[test]
fn resolves_bus_to_controller_and_port() {
    let report = analyze(vec![I2cDevice {
        bus: Some("bus2".into()),
        ..base_device()
    }])
    .unwrap();
    assert_eq!(report.devices[0].location.controller, 2);
    assert_eq!(report.devices[0].location.index, 1);

    let report = analyze(vec![I2cDevice {
        bus: Some("bus1".into()),
        ..base_device()
    }])
    .unwrap();
    assert_eq!(report.devices[0].location.controller, 2);
    assert_eq!(report.devices[0].location.index, 0);
}

#[test]
fn resolves_explicit_port() {
    let report = analyze(vec![I2cDevice {
        controller: Some(2),
        port: Some("F".into()),
        ..base_device()
    }])
    .unwrap();
    assert_eq!(report.devices[0].location.controller, 2);
    assert_eq!(report.devices[0].location.index, 1);
}

#[test]
fn resolves_singleton_port() {
    // Controller 3 has exactly one port, so naming a port is optional.
    let report = analyze(vec![I2cDevice {
        controller: Some(3),
        ..base_device()
    }])
    .unwrap();
    assert_eq!(report.devices[0].location.controller, 3);
    assert_eq!(report.devices[0].location.index, 0);
}

#[test]
fn resolves_mux_and_segment() {
    let report = analyze(vec![I2cDevice {
        controller: Some(3),
        mux: Some(1),
        segment: Some(4),
        ..base_device()
    }])
    .unwrap();
    assert_eq!(
        report.devices[0].segment,
        Some(MuxSegment { mux: 1, segment: 4 })
    );

    let report = analyze(vec![I2cDevice {
        controller: Some(3),
        ..base_device()
    }])
    .unwrap();
    assert_eq!(report.devices[0].segment, None);
}

#[test]
fn registers_buses_from_every_controller() {
    // Buses are registered from all controllers, even those that don't match
    // our role, so that devices can always find their bus.
    let report = analyze_with(
        vec![I2cDevice {
            bus: Some("bus1".into()),
            ..base_device()
        }],
        settings(ControllerRole::Target),
    )
    .unwrap();

    assert!(report.controllers.is_empty());
    assert_eq!(report.devices[0].location.controller, 2);
    assert_eq!(report.ports.len(), 3);
    assert_eq!(report.buses.len(), 3);
}

//
// Topology errors
//

#[test]
fn error_no_bus_or_controller() {
    assert_error(vec![base_device()], "must have a bus or controller");
}

#[test]
fn error_both_bus_and_controller() {
    assert_error(
        vec![I2cDevice {
            controller: Some(2),
            bus: Some("bus1".into()),
            ..base_device()
        }],
        "has both a bus and a controller",
    );
}

#[test]
fn error_unknown_bus() {
    assert_error(
        vec![I2cDevice {
            bus: Some("nonesuch".into()),
            ..base_device()
        }],
        "specifies unknown bus \"nonesuch\"",
    );
}

#[test]
fn error_both_port_and_bus() {
    assert_error(
        vec![I2cDevice {
            bus: Some("bus1".into()),
            port: Some("B".into()),
            ..base_device()
        }],
        "has both port and bus",
    );
}

#[test]
fn error_invalid_port() {
    assert_error(
        vec![I2cDevice {
            controller: Some(2),
            port: Some("Q".into()),
            ..base_device()
        }],
        "has invalid port",
    );
}

#[test]
fn error_ambiguous_port() {
    // Controller 2 has two ports, so one must be named.
    assert_error(
        vec![I2cDevice {
            controller: Some(2),
            ..base_device()
        }],
        "has ambiguous port",
    );
}

#[test]
fn error_mux_without_segment() {
    assert_error(
        vec![I2cDevice {
            controller: Some(3),
            mux: Some(1),
            ..base_device()
        }],
        "specifies a mux but no segment",
    );
}

#[test]
fn error_segment_without_mux() {
    assert_error(
        vec![I2cDevice {
            controller: Some(3),
            segment: Some(1),
            ..base_device()
        }],
        "specifies a segment but no mux",
    );
}

#[test]
fn error_mux_zero() {
    assert_error(
        vec![I2cDevice {
            controller: Some(3),
            mux: Some(0),
            segment: Some(1),
            ..base_device()
        }],
        "invalid mux value of 0",
    );
}

#[test]
fn error_mux_out_of_range() {
    // Controller 3's only port has exactly one mux.
    assert_error(
        vec![I2cDevice {
            controller: Some(3),
            mux: Some(2),
            segment: Some(1),
            ..base_device()
        }],
        "invalid mux 2",
    );
}

#[test]
fn error_bus_appears_twice() {
    let mut all_controllers = controllers();
    all_controllers.push(common::controller(
        4,
        [(
            "C",
            common::port(
                Some("bus1"),
                common::pin(None, 1),
                common::pin(None, 2),
                4,
                vec![],
            ),
        )],
    ));

    let err = analyze_manifest(
        all_controllers,
        None,
        None,
        settings(ControllerRole::Initiator),
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("i2c bus bus1 appears twice"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn error_eeprom_vpd_on_non_eeprom() {
    assert_error(
        vec![I2cDevice {
            bus: Some("bus1".into()),
            eeprom_vpd: Some(EepromVpd::SingleBarcode),
            ..base_device()
        }],
        "is not a supported EEPROM device",
    );
}

//
// Naming errors
//

#[test]
fn error_duplicate_name() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            name: Some("north".into()),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            name: Some("north".into()),
            ..base_device()
        },
    ];
    assert_error(devices, "duplicate name north for device tmp117");
}

#[test]
fn error_duplicate_refdes() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            refdes: Some(Refdes::Component("U1".into())),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            refdes: Some(Refdes::Component("U1".into())),
            ..base_device()
        },
    ];
    assert_error(devices, "duplicate refdes");
}

#[test]
fn error_refdes_collides_with_name() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            name: Some("U1".into()),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            refdes: Some(Refdes::Component("U1".into())),
            ..base_device()
        },
    ];
    assert_error(devices, "is also a device name");
}

//
// Power errors
//

fn power_device(power: I2cPower) -> I2cDevice {
    I2cDevice {
        bus: Some("bus1".into()),
        power: Some(power),
        ..common::device("raa229618", 0x35, "a power controller")
    }
}

#[test]
fn resolves_power_rails() {
    let report = analyze(vec![power_device(I2cPower {
        rails: Some(vec!["V1".into(), "V2".into()]),
        ..Default::default()
    })])
    .unwrap();

    assert_eq!(report.rails.pmbus.len(), 2);
    assert_eq!(report.rails.pmbus[0].rail, "V1");
    assert_eq!(report.rails.pmbus[0].bank, Some(0));
    assert_eq!(report.rails.pmbus[1].rail, "V2");
    assert_eq!(report.rails.pmbus[1].bank, Some(1));

    // These rails are on a PMBus device, so they are not in `power`.
    assert!(report.rails.non_pmbus.is_empty());

    // A single rail has no bank...
    let report = analyze(vec![power_device(I2cPower {
        rails: Some(vec!["V1".into()]),
        ..Default::default()
    })])
    .unwrap();
    assert_eq!(report.rails.pmbus[0].bank, None);

    // ...and a non-PMBus device's rails appear in both lists.
    let report = analyze(vec![power_device(I2cPower {
        rails: Some(vec!["V1".into()]),
        pmbus: false,
        ..Default::default()
    })])
    .unwrap();
    assert_eq!(report.rails.pmbus.len(), 1);
    assert_eq!(report.rails.non_pmbus.len(), 1);
}

#[test]
fn error_rail_phase_length_mismatch() {
    assert_error(
        vec![power_device(I2cPower {
            rails: Some(vec!["V1".into(), "V2".into()]),
            phases: Some(vec![vec![0, 1]]),
            ..Default::default()
        })],
        "rail/phase length mismatch",
    );
}

#[test]
fn error_duplicate_phase() {
    assert_error(
        vec![power_device(I2cPower {
            rails: Some(vec!["V1".into(), "V2".into()]),
            phases: Some(vec![vec![0, 1], vec![1]]),
            ..Default::default()
        })],
        "phase 1 appears multiple times",
    );
}

#[test]
fn error_duplicate_rail() {
    let devices = vec![
        power_device(I2cPower {
            rails: Some(vec!["V1".into()]),
            ..Default::default()
        }),
        power_device(I2cPower {
            rails: Some(vec!["V1".into()]),
            ..Default::default()
        }),
    ];
    assert_error(devices, "duplicate rail V1");
}

//
// Sensor errors
//

#[test]
fn error_sensor_count_exceeds_rails() {
    let devices = vec![I2cDevice {
        sensors: Some(I2cSensors {
            voltage: 2,
            ..Default::default()
        }),
        ..power_device(I2cPower {
            rails: Some(vec!["V1".into()]),
            ..Default::default()
        })
    }];
    assert_error(devices, "sensor count exceeds rails");
}

#[test]
fn error_name_array_too_short() {
    let devices = vec![I2cDevice {
        bus: Some("bus1".into()),
        sensors: Some(I2cSensors {
            temperature: 2,
            names: Some(vec!["north".into()]),
            ..Default::default()
        }),
        ..base_device()
    }];
    assert_error(devices, "name array is too short (1) for sensor index (1)");
}

#[test]
fn error_inconsistent_sensors() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            description: "one".into(),
            sensors: Some(I2cSensors {
                temperature: 1,
                ..Default::default()
            }),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            address: 0x49,
            description: "two".into(),
            sensors: Some(I2cSensors {
                temperature: 2,
                ..Default::default()
            }),
            ..base_device()
        },
    ];
    assert_error(devices, "inconsistent numbers of sensors");
}

#[test]
fn flavor_disambiguates_inconsistent_sensors() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            description: "one".into(),
            sensors: Some(I2cSensors {
                temperature: 1,
                ..Default::default()
            }),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            address: 0x49,
            description: "two".into(),
            flavor: Some("double".into()),
            sensors: Some(I2cSensors {
                temperature: 2,
                ..Default::default()
            }),
            ..base_device()
        },
    ];
    let report = analyze(devices).unwrap();
    assert_eq!(report.sensor_structs[0].name, "tmp117");
    assert!(report.sensor_structs[0].declare);
    assert_eq!(report.sensor_structs[1].name, "tmp117_double");
    assert!(report.sensor_structs[1].declare);
}

#[test]
fn error_with_and_without_sensors() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            description: "one".into(),
            sensors: Some(I2cSensors {
                temperature: 1,
                ..Default::default()
            }),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            address: 0x49,
            description: "two".into(),
            ..base_device()
        },
    ];
    assert_error(devices, "declared both with and without sensors");
}

#[test]
fn sensor_ids_are_assigned_in_order() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            description: "one".into(),
            name: Some("north".into()),
            sensors: Some(I2cSensors {
                temperature: 2,
                voltage: 1,
                ..Default::default()
            }),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            address: 0x49,
            description: "two".into(),
            name: Some("south".into()),
            sensors: Some(I2cSensors {
                temperature: 2,
                voltage: 1,
                ..Default::default()
            }),
            ..base_device()
        },
    ];
    let report = analyze(devices).unwrap();
    assert_eq!(report.sensors.total_i2c_sensors, 6);

    //
    // Sensors are numbered device by device, and within a device in the order
    // temperature, power, current, voltage, ...
    //
    let ids: Vec<_> = report
        .sensors
        .by_id
        .iter()
        .map(|s| (s.id, s.kind.to_string(), s.name.clone()))
        .collect();

    assert_eq!(
        ids,
        vec![
            (0, "TEMPERATURE".to_string(), Some("north".to_string())),
            (1, "TEMPERATURE".to_string(), Some("north".to_string())),
            (2, "VOLTAGE".to_string(), Some("north".to_string())),
            (3, "TEMPERATURE".to_string(), Some("south".to_string())),
            (4, "TEMPERATURE".to_string(), Some("south".to_string())),
            (5, "VOLTAGE".to_string(), Some("south".to_string())),
        ]
    );
}

#[test]
fn power_sensors_subset_names_only_those_kinds() {
    //
    // `power.sensors` limits which sensor kinds are named after rails; other
    // kinds fall back to the `names` array.
    //
    let devices = vec![I2cDevice {
        bus: Some("bus1".into()),
        name: Some("psu".into()),
        power: Some(I2cPower {
            rails: Some(vec!["V54_PSU".into()]),
            sensors: Some(vec![Sensor::Voltage]),
            ..Default::default()
        }),
        sensors: Some(I2cSensors {
            temperature: 2,
            voltage: 1,
            names: Some(vec!["inlet".into(), "outlet".into()]),
            ..Default::default()
        }),
        ..common::device("mwocp68", 0x40, "a power shelf")
    }];
    let report = analyze(devices).unwrap();

    let names: Vec<_> = report
        .sensors
        .by_id
        .iter()
        .map(|s| (s.kind.to_string(), s.name.clone().unwrap()))
        .collect();

    assert_eq!(
        names,
        vec![
            ("TEMPERATURE".to_string(), "inlet".to_string()),
            ("TEMPERATURE".to_string(), "outlet".to_string()),
            ("VOLTAGE".to_string(), "V54_PSU".to_string()),
        ]
    );
}

//
// Roles
//

fn target_controller(n: u8) -> I2cController {
    I2cController {
        target: true,
        ..common::controller(
            n,
            [(
                "B",
                common::port(
                    None,
                    common::pin(None, 1),
                    common::pin(None, 2),
                    4,
                    vec![],
                ),
            )],
        )
    }
}

#[test]
fn role_selects_controllers() {
    let mut all_controllers = controllers();
    all_controllers.push(target_controller(7));

    let initiator = analyze_manifest(
        all_controllers.clone(),
        None,
        None,
        settings(ControllerRole::Initiator),
    )
    .unwrap();
    assert_eq!(initiator.controllers.len(), 2);
    initiator.check_single_controller().unwrap_err();

    let target = analyze_manifest(
        all_controllers,
        None,
        None,
        settings(ControllerRole::Target),
    )
    .unwrap();
    assert_eq!(target.controllers.len(), 1);
    assert_eq!(target.controllers[0].controller, 7);
    target.check_single_controller().unwrap();
}

#[test]
fn error_no_target_controller() {
    let report = analyze_manifest(
        controllers(),
        None,
        None,
        settings(ControllerRole::Target),
    )
    .unwrap();

    let err = report.check_single_controller().unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "found 0 I2C controller(s); expected exactly one"
    );
}

#[test]
fn error_two_target_controllers() {
    let mut all_controllers = controllers();
    all_controllers.push(target_controller(7));
    all_controllers.push(target_controller(8));

    let report = analyze_manifest(
        all_controllers,
        None,
        None,
        settings(ControllerRole::Target),
    )
    .unwrap();

    let err = report.check_single_controller().unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "found 2 I2C controller(s); expected exactly one"
    );
}

//
// Validation
//

fn validation_settings(drivers: &[&str]) -> AnalysisSettings {
    AnalysisSettings {
        role: ControllerRole::Initiator,
        component_ids: false,
        drivers: Some(
            drivers
                .iter()
                .map(|d| d.to_string())
                .collect::<HashSet<_>>(),
        ),
        ..Default::default()
    }
}

#[test]
fn validation_strategies() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            validate_with_raw_read: true,
            ..common::device("nonesuch", 0x20, "a device with no driver")
        },
    ];

    let report =
        analyze_with(devices, validation_settings(&["tmp117"])).unwrap();

    let validation = report.validation.unwrap();
    assert_eq!(
        validation,
        vec![
            analysis::Validation::Driver("Tmp117".to_string()),
            analysis::Validation::RawRead,
        ]
    );
}

#[test]
fn error_driver_with_raw_read() {
    let err = analyze_with(
        vec![I2cDevice {
            bus: Some("bus1".into()),
            validate_with_raw_read: true,
            ..base_device()
        }],
        validation_settings(&["tmp117"]),
    )
    .unwrap_err();

    assert!(
        format!("{err:#}")
            .contains("set `validate-with-raw-read = true`, but that was"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn error_no_driver_without_raw_read() {
    let err = analyze_with(
        vec![I2cDevice {
            bus: Some("bus1".into()),
            ..base_device()
        }],
        validation_settings(&[]),
    )
    .unwrap_err();

    assert!(
        format!("{err:#}").contains("has no driver in `drv-i2c-devices`"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn validation_is_opt_in() {
    // Without drivers, no validation analysis is performed -- and so a device
    // with no driver is not an error.
    let report = analyze(vec![I2cDevice {
        bus: Some("bus1".into()),
        ..base_device()
    }])
    .unwrap();
    assert!(report.validation.is_none());
}

//
// Groupings
//

#[test]
fn groups_devices() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            name: Some("north".into()),
            refdes: Some(Refdes::Component("U1".into())),
            description: "one".into(),
            ..base_device()
        },
        I2cDevice {
            bus: Some("bus2".into()),
            address: 0x49,
            name: Some("south".into()),
            description: "two".into(),
            ..base_device()
        },
        I2cDevice {
            controller: Some(3),
            eeprom_vpd: Some(EepromVpd::SingleBarcode),
            ..common::device("at24csw080", 0x50, "three")
        },
    ];
    let report = analyze(devices).unwrap();

    assert_eq!(
        report.by_device,
        vec![
            common::group("at24csw080".to_string(), &[2]),
            common::group("tmp117".to_string(), &[0, 1]),
        ]
    );
    assert_eq!(
        report.by_bus,
        vec![
            common::group(
                DeviceBus {
                    device: "tmp117".to_string(),
                    bus: "bus1".to_string()
                },
                &[0]
            ),
            common::group(
                DeviceBus {
                    device: "tmp117".to_string(),
                    bus: "bus2".to_string()
                },
                &[1]
            ),
        ]
    );
    assert_eq!(
        report.by_controller,
        vec![common::group(2, &[0, 1]), common::group(3, &[2])]
    );
    assert_eq!(
        report.by_port,
        vec![common::group(0, &[0, 2]), common::group(1, &[1])]
    );
    assert_eq!(report.max_component_id_len, 2);
}

#[test]
fn component_ids_are_resolved_on_request() {
    let d = I2cDevice {
        bus: Some("bus1".into()),
        refdes: Some(Refdes::Path(vec!["J1".into(), "U7".into()])),
        ..base_device()
    };

    let report = analyze(vec![d.clone()]).unwrap();
    assert_eq!(report.devices[0].component_id, None);

    let report = analyze_with(
        vec![d],
        AnalysisSettings {
            component_ids: true,
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap();
    assert_eq!(report.devices[0].component_id.as_deref(), Some("J1/U7"));
}

#[test]
fn error_duplicate_component_id_across_device_types() {
    // The same refdes on two different device types is still one component.
    let err = analyze(vec![
        I2cDevice {
            refdes: Some(Refdes::Component("U7".into())),
            bus: Some("bus1".into()),
            ..base_device()
        },
        I2cDevice {
            refdes: Some(Refdes::Component("U7".into())),
            bus: Some("bus2".into()),
            ..common::device("at24csw080", 0x50, "an eeprom")
        },
    ])
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("duplicate component ID \"U7\""), "{msg}");
    assert!(msg.contains("1 component ID problem(s)"), "{msg}");
}

#[test]
fn component_ids_are_optional_by_default() {
    let devices = vec![I2cDevice {
        bus: Some("bus1".into()),
        description: "a temperature sensor without a refdes".into(),
        ..base_device()
    }];
    let report =
        analyze_with(devices.clone(), settings(ControllerRole::Initiator))
            .unwrap();
    let descs: Vec<_> = report.device_descriptions().collect();
    assert_eq!(descs[0].device_id, None);

    let err = analyze_with(
        devices,
        AnalysisSettings {
            require_component_ids: true,
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("has no component ID (refdes)"),
        "{err:#}"
    );
}

#[test]
fn error_component_id_too_long() {
    let d = I2cDevice {
        refdes: Some(Refdes::Path(vec!["J1".into(), "U7".into()])),
        bus: Some("bus1".into()),
        ..base_device()
    };

    // "J1/U7" is 5 bytes.
    analyze_with(
        vec![d.clone()],
        AnalysisSettings {
            max_component_id_len: Some(5),
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap();

    let err = analyze_with(
        vec![d],
        AnalysisSettings {
            max_component_id_len: Some(4),
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap_err();
    assert!(
        format!("{err:#}")
            .contains("component ID \"J1/U7\" for device \"tmp117\" exceeds"),
        "{err:#}"
    );
}

#[test]
fn all_component_id_problems_are_reported_together() {
    let devices = vec![
        I2cDevice {
            bus: Some("bus1".into()),
            description: "no refdes".into(),
            ..base_device()
        },
        I2cDevice {
            refdes: Some(Refdes::Component("U1".into())),
            bus: Some("bus1".into()),
            address: 0x49,
            description: "first U1".into(),
            ..base_device()
        },
        I2cDevice {
            refdes: Some(Refdes::Component("U1".into())),
            bus: Some("bus2".into()),
            ..common::device("at24csw080", 0x50, "second U1")
        },
        I2cDevice {
            refdes: Some(Refdes::Path(vec!["J100".into(), "U200".into()])),
            bus: Some("bus2".into()),
            address: 0x4a,
            description: "too long".into(),
            ..base_device()
        },
    ];
    let err = analyze_with(
        devices,
        AnalysisSettings {
            require_component_ids: true,
            max_component_id_len: Some(8),
            ..settings(ControllerRole::Initiator)
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("3 component ID problem(s)"), "{msg}");
    assert!(msg.contains("has no component ID"), "{msg}");
    assert!(msg.contains("duplicate component ID \"U1\""), "{msg}");
    assert!(
        msg.contains("\"J100/U200\" for device \"tmp117\" exceeds"),
        "{msg}"
    );
}

#[test]
fn vpd_kind_is_classified_by_device_type() {
    let report = analyze(vec![
        I2cDevice {
            refdes: Some(Refdes::Component("U1".into())),
            bus: Some("bus1".into()),
            ..common::device("at24csw080", 0x50, "default eeprom format")
        },
        I2cDevice {
            refdes: Some(Refdes::Component("U2".into())),
            bus: Some("bus1".into()),
            eeprom_vpd: Some(EepromVpd::SledFanTray),
            ..common::device("at24csw080", 0x51, "fan tray eeprom")
        },
        I2cDevice {
            refdes: Some(Refdes::Component("U3".into())),
            bus: Some("bus1".into()),
            ..common::device("tmp117", 0x48, "tmp11x")
        },
        I2cDevice {
            refdes: Some(Refdes::Component("U4".into())),
            bus: Some("bus1".into()),
            ..common::device("tmp116", 0x49, "tmp11x too")
        },
        I2cDevice {
            refdes: Some(Refdes::Component("U5".into())),
            bus: Some("bus2".into()),
            ..common::device("max31790", 0x20, "no vpd")
        },
    ])
    .unwrap();
    let vpd: Vec<_> = report.device_descriptions().map(|d| d.vpd).collect();
    assert_eq!(
        vpd,
        [
            Some(VpdKind::Eeprom(EepromVpd::SingleBarcode)),
            Some(VpdKind::Eeprom(EepromVpd::SledFanTray)),
            Some(VpdKind::Tmp11x),
            Some(VpdKind::Tmp11x),
            None,
        ]
    );
}

fn other_sensor_devices() -> Vec<OtherSensorDevice> {
    vec![
        OtherSensorDevice {
            name: "dimm_a".into(),
            device: "ts0".into(),
            description: "DIMM A".into(),
            sensors: BTreeMap::from([(Sensor::Temperature, 2)]),
            refdes: Some(Refdes::Component("J1".into())),
        },
        OtherSensorDevice {
            name: "fans".into(),
            device: "fpga".into(),
            description: "fan hub".into(),
            sensors: BTreeMap::from([
                (Sensor::Speed, 3),
                (Sensor::Temperature, 1),
            ]),
            refdes: None,
        },
    ]
}

#[test]
fn other_sensors_follow_i2c_sensor_ids() {
    let report = analyze_with_sensor(
        vec![I2cDevice {
            name: Some("north".into()),
            refdes: Some(Refdes::Component("U7".into())),
            bus: Some("bus1".into()),
            description: "an i2c temperature sensor".into(),
            sensors: Some(I2cSensors {
                temperature: 1,
                ..Default::default()
            }),
            ..base_device()
        }],
        Some(SensorConfig {
            devices: other_sensor_devices(),
        }),
    )
    .unwrap();

    let s = &report.sensors;
    assert_eq!(s.total_i2c_sensors, 1);
    assert_eq!(s.total_other_sensors, 6);
    assert_eq!(s.by_id.len(), 7);

    // Non-I2C IDs continue after the I2C ones, in manifest order, and by
    // kind within a device (in `Sensor` order: temperature before speed).
    assert_eq!(s.other_sensors.len(), 2);
    let dimm = &s.other_sensors[0];
    assert_eq!(dimm.config.name, "dimm_a");
    assert_eq!(dimm.ids_by_kind[&Sensor::Temperature], vec![1, 2]);
    let fans = &s.other_sensors[1];
    assert_eq!(fans.ids_by_kind[&Sensor::Temperature], vec![3]);
    assert_eq!(fans.ids_by_kind[&Sensor::Speed], vec![4, 5, 6]);

    // They are all visible by ID, with their name and refdes.
    let sensor = s.by_id.get(&2).unwrap();
    assert_eq!(sensor.name.as_deref(), Some("dimm_a"));
    assert_eq!(sensor.kind, Sensor::Temperature);
    assert_eq!(
        sensor.refdes,
        Some(build_i2c::Refdes::Component("J1".into()))
    );
    assert_eq!(s.by_id.get(&6).unwrap().refdes, None);
}

#[test]
fn error_duplicate_other_sensor_name() {
    let err = analyze_with_sensor(
        vec![],
        Some(SensorConfig {
            devices: vec![
                OtherSensorDevice {
                    name: "dimm_a".into(),
                    device: "ts0".into(),
                    description: "DIMM A".into(),
                    sensors: BTreeMap::from([(Sensor::Temperature, 1)]),
                    refdes: None,
                },
                OtherSensorDevice {
                    name: "dimm_a".into(),
                    device: "ts1".into(),
                    description: "DIMM A again".into(),
                    sensors: BTreeMap::from([(Sensor::Temperature, 1)]),
                    refdes: None,
                },
            ],
        }),
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("Duplicate sensor name: dimm_a"),
        "{err:#}"
    );
}
