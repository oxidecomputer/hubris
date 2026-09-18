// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Shared constructor helpers for the `analysis` and `codegen` test suites.
//!
//! These are pure bookkeeping: they assemble the `load` and `analysis` types
//! from values the tests already know, without re-implementing any of the
//! decisions `analysis::analyze` makes (bus/port resolution, sensor naming,
//! grouping, ...). Tests that need those decisions state the resolved facts
//! themselves; these helpers just save the struct-literal boilerplate.

#![allow(dead_code)]

use build_i2c::analysis::{
    ControllerPort, Device, DeviceGroup, DeviceKey, DeviceLookup,
    DeviceNameKey, DeviceRefdesKey, DeviceSensor, OtherSensors,
    SensorsDescription,
};
use build_i2c::load::{
    I2cController, I2cDevice, I2cGpio, I2cMux, I2cPin, I2cPort,
};
use std::sync::Arc;

/// Builds an `I2cController` from its number and named ports.
pub fn controller(
    n: u8,
    ports: impl IntoIterator<Item = (&'static str, I2cPort)>,
) -> I2cController {
    I2cController {
        controller: n,
        ports: ports
            .into_iter()
            .map(|(name, port)| (name.to_string(), port))
            .collect(),
        target: false,
    }
}

/// Builds an `I2cPort`.
pub fn port(
    name: Option<&str>,
    scl: I2cPin,
    sda: I2cPin,
    af: u8,
    muxes: Vec<I2cMux>,
) -> I2cPort {
    I2cPort {
        name: name.map(String::from),
        description: None,
        scl,
        sda,
        af,
        muxes,
    }
}

/// Builds an `I2cPin`.
pub fn pin(gpio_port: Option<&str>, n: u8) -> I2cPin {
    I2cPin {
        gpio_port: gpio_port.map(String::from),
        pin: n,
    }
}

/// Builds an `I2cGpio`.
pub fn gpio(port: &str, pin: u8) -> I2cGpio {
    I2cGpio {
        port: port.to_string(),
        pin,
    }
}

/// Builds an `I2cMux`.
pub fn mux(driver: &str, address: u8, nreset: Option<I2cGpio>) -> I2cMux {
    I2cMux {
        driver: driver.to_string(),
        address,
        nreset,
    }
}

/// Builds a minimal `I2cDevice`, with every other field defaulted; tests
/// override whatever else they need with struct-update syntax.
pub fn device(kind: &str, address: u8, description: &str) -> I2cDevice {
    I2cDevice {
        device: kind.to_string(),
        address,
        description: description.to_string(),
        ..Default::default()
    }
}

/// Builds a [`Device`] (the analyzed form of an `I2cDevice`) already resolved
/// onto a controller and port, with no segment, component ID, or PMBus
/// description. Tests override those with struct-update syntax.
pub fn resolved(config: I2cDevice, controller: u8, index: usize) -> Device {
    Device {
        config,
        location: ControllerPort { controller, index },
        segment: None,
        component_id: None,
        pmbus: None,
    }
}

/// Builds a [`DeviceGroup`].
pub fn group<K>(key: K, indices: &[usize]) -> DeviceGroup<K> {
    DeviceGroup {
        key,
        indices: indices.to_vec(),
    }
}

/// Builds a [`DeviceLookup`].
pub fn lookup<K>(key: K, index: usize) -> DeviceLookup<K> {
    DeviceLookup { key, index }
}

/// Builds a [`SensorsDescription`] from explicit, already-resolved sensor
/// entries.
///
/// `device_kinds` is the device type (e.g. `"tmp117"`) for each index into
/// [`build_i2c::analysis::Report::devices`]; `entries` are `(device index,
/// sensor)` pairs, in the order sensors were allocated. This only performs
/// the bookkeeping that `SensorsDescription::new` does (filling in
/// `by_device`, `by_name`, `by_refdes`, `by_id`, `device_sensors`, and the
/// sensor-count totals): it does not decide sensor names, kinds, or IDs --
/// the `entries` already carry those decisions.
pub fn sensors_description(
    device_kinds: &[&str],
    entries: &[(usize, DeviceSensor)],
    other_sensors: Vec<OtherSensors>,
) -> SensorsDescription {
    let mut desc = SensorsDescription {
        device_sensors: vec![Vec::new(); device_kinds.len()],
        ..Default::default()
    };

    for (device_index, sensor) in entries {
        let sensor = Arc::new(sensor.clone());
        let device = device_kinds[*device_index].to_string();

        desc.by_device
            .entry(DeviceKey {
                device: device.clone(),
                kind: sensor.kind,
            })
            .or_default()
            .push(sensor.id);

        if let Some(name) = &sensor.name {
            desc.by_name
                .entry(DeviceNameKey {
                    device: device.clone(),
                    name: name.clone(),
                    kind: sensor.kind,
                })
                .or_default()
                .push(sensor.id);
        }

        if let Some(refdes) = &sensor.refdes {
            desc.by_refdes
                .entry(DeviceRefdesKey {
                    device: device.clone(),
                    refdes: refdes.clone(),
                    kind: sensor.kind,
                })
                .or_default()
                .push(sensor.id);
        }

        desc.by_id
            .insert_unique(sensor.clone())
            .expect("unique sensor id in fixture");
        desc.device_sensors[*device_index].push(sensor);
    }
    desc.total_i2c_sensors = entries.len();

    for other in &other_sensors {
        for (&kind, ids) in &other.ids_by_kind {
            for &id in ids {
                let sensor = Arc::new(DeviceSensor {
                    refdes: other.config.refdes.clone(),
                    name: Some(other.config.name.clone()),
                    kind,
                    id,
                });
                desc.by_id
                    .insert_unique(sensor)
                    .expect("unique sensor id in fixture");
                desc.total_other_sensors += 1;
            }
        }
    }
    desc.other_sensors = other_sensors;

    desc
}
