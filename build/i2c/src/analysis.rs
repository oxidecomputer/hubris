// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Stage 2: analysis of a loaded configuration.

use crate::load::{I2cDevice, Refdes, Sensor};
use iddqd::IdOrdMap;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Clone)]
pub struct DeviceKey {
    pub device: String,
    pub kind: Sensor,
}

#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Clone)]
pub struct DeviceNameKey {
    pub device: String,
    pub name: String,
    pub kind: Sensor,
}

#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Clone)]
pub struct DeviceRefdesKey {
    pub device: String,
    pub refdes: Refdes,
    pub kind: Sensor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSensor {
    pub refdes: Option<Refdes>,
    pub name: Option<String>,
    pub kind: Sensor,
    pub id: usize,
}

impl iddqd::IdOrdItem for DeviceSensor {
    type Key<'a> = &'a usize;

    /// Retrieves the key.
    fn key(&self) -> Self::Key<'_> {
        &self.id
    }

    iddqd::id_upcast!();
}

#[derive(Debug)]
pub struct I2cSensorsDescription {
    // In all maps below, the value is the sensor ID. The same sensor ID
    // can show up in multiple (including all!) of these maps.
    //
    // All sensors are guaranteed to be present in `by_device`, but
    // may not be present in the other maps (devices may or may not have a
    // name/bus in app.toml).
    /// `by_device` tracks items by what the sensor is (e.g. "Temperature"),
    /// and by what kind of device the sensor exists within, e.g. "TMP117".
    /// A `(Temperature, TMP117)` may have multiple sensor IDs that match the
    /// same tuple of options.
    pub(crate) by_device: BTreeMap<DeviceKey, Vec<usize>>,
    /// `by_refdes` tracks items on the two items above, PLUS what "refdes"
    /// is available, for example "U32". A `(Speed, Max31790, U32)` may
    /// have multiple sensor IDs, for example if there are 6 separate speed
    /// sensors hosted on the same physical I2C device.
    pub(crate) by_refdes: BTreeMap<DeviceRefdesKey, Vec<usize>>,
    /// `by_name` tracks items on the triple of device/name/kind, for example
    /// `(TMP117, "North", Temperature)`. This actually should probably not
    /// ever match multiple items, and will be fixed in the future. See
    /// https://github.com/oxidecomputer/hubris/issues/2637 for details.
    pub(crate) by_name: BTreeMap<DeviceNameKey, Vec<usize>>,
    /// All sensors by their ID
    pub by_id: IdOrdMap<Arc<DeviceSensor>>,

    /// list of all devices and a list of their sensors, with an optional sensor
    /// name (if present)
    pub(crate) device_sensors: Vec<Vec<Arc<DeviceSensor>>>,

    pub total_sensors: usize,
}

impl std::fmt::Display for I2cSensorsDescription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let I2cSensorsDescription {
            by_device,
            by_name,
            by_refdes,
            by_id,
            device_sensors,
            total_sensors,
        } = self;
        writeln!(f, "by_device:")?;
        for (k, vs) in by_device {
            writeln!(f, "  - {k:?} :: {vs:?}")?;
        }
        writeln!(f, "by_name:")?;
        for (k, vs) in by_name {
            writeln!(f, "  - {k:?} :: {vs:?}")?;
        }
        writeln!(f, "by_refdes:")?;
        for (k, vs) in by_refdes {
            writeln!(f, "  - {k:?} :: {vs:?}")?;
        }
        writeln!(f, "by_id:")?;
        for s in by_id {
            writeln!(f, "  - {s:?}")?;
        }
        writeln!(f, "device_sensors:")?;
        for (i, d) in device_sensors.iter().enumerate() {
            writeln!(f, "  - I2cDevice({i})")?;
            for (j, s) in d.iter().enumerate() {
                writeln!(f, "    - {i}.{j}: {s:?}")?;
            }
        }
        writeln!(f, "total_sensors: {total_sensors}")
    }
}

impl I2cSensorsDescription {
    pub(crate) fn new(devices: &[I2cDevice]) -> Self {
        let mut desc = Self {
            by_device: BTreeMap::new(),
            by_name: BTreeMap::new(),
            by_refdes: BTreeMap::new(),
            by_id: IdOrdMap::new(),
            device_sensors: vec![Vec::new(); devices.len()],
            total_sensors: 0,
        };

        for (d_index, d) in devices.iter().enumerate() {
            if let Some(s) = &d.sensors {
                for i in 0..s.temperature {
                    desc.add_sensor(Sensor::Temperature, d, i, d_index);
                }

                for i in 0..s.power {
                    desc.add_sensor(Sensor::Power, d, i, d_index);
                }

                for i in 0..s.current {
                    desc.add_sensor(Sensor::Current, d, i, d_index);
                }

                for i in 0..s.voltage {
                    desc.add_sensor(Sensor::Voltage, d, i, d_index);
                }

                for i in 0..s.input_current {
                    desc.add_sensor(Sensor::InputCurrent, d, i, d_index);
                }

                for i in 0..s.input_voltage {
                    desc.add_sensor(Sensor::InputVoltage, d, i, d_index);
                }

                for i in 0..s.speed {
                    desc.add_sensor(Sensor::Speed, d, i, d_index);
                }
            }
        }

        desc
    }

    // `idx` is the index of the type of sensor within `d` (the idx-th
    // temperature sensor or the idx-th power sensor, etc.; see the loop in
    // `new()` above).
    //
    // `dev_index` is the index of `d` within the total list of devices.
    //
    // This method should only be called by `new()`. It fills out `self`'s
    // fields as it is being constructed.
    fn add_sensor(
        &mut self,
        kind: Sensor,
        d: &I2cDevice,
        idx: usize,
        dev_index: usize,
    ) {
        let id = self.total_sensors;
        self.total_sensors += 1;

        let name: Option<String> = if let Some(power) = d.power_for_kind(kind) {
            if let Some(rails) = &power.rails {
                if idx < rails.len() {
                    Some(rails[idx].clone())
                } else {
                    panic!("sensor count exceeds rails for {d:?}",);
                }
            } else {
                d.name.clone()
            }
        } else if let Some(names) = &d.sensors.as_ref().unwrap().names {
            if idx >= names.len() {
                panic!(
                    "name array is too short ({}) for sensor index ({idx})",
                    names.len(),
                );
            } else {
                Some(names[idx].clone())
            }
        } else {
            d.name.clone()
        };

        if let Some(name) = name.clone() {
            self.by_name
                .entry(DeviceNameKey {
                    device: d.device.clone(),
                    name,
                    kind,
                })
                .or_default()
                .push(id);
        }

        let sensor = Arc::new(DeviceSensor {
            refdes: d.refdes.clone(),
            name: name.clone(),
            kind,
            id,
        });

        if let Some(ref refdes) = sensor.refdes {
            self.by_refdes
                .entry(DeviceRefdesKey {
                    device: d.device.clone(),
                    refdes: refdes.clone(),
                    kind,
                })
                .or_default()
                .push(id);
        }

        self.by_device
            .entry(DeviceKey {
                device: d.device.clone(),
                kind,
            })
            .or_default()
            .push(id);

        if let Err(prev) = self.by_id.insert_unique(sensor.clone()) {
            panic!("weird: colliding sensor ID {id}: {prev:?} and {sensor:?}",);
        };

        self.device_sensors[dev_index].push(sensor);
    }
}
