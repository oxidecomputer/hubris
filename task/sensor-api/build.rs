// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::{fmt::Write as FmtWrite, io::Write as IoWrite};

/// This represents our _subset_ of global config and _must not_ be marked with
/// `deny_unknown_fields`!
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct GlobalConfig {
    sensor: Option<SensorConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SensorConfig {
    devices: Vec<Sensor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Sensor {
    name: String,
    device: String,
    description: String,
    sensors: BTreeMap<String, usize>,
}

fn main() -> Result<()> {
    idol::client::build_client_stub("../../idl/sensor.idol", "client_stub.rs")
        .map_err(|e| anyhow!("idol error: {e}"))?;

    build_i2c::codegen(build_i2c::Disposition::Sensors)?;

    let config: GlobalConfig = build_util::config()?;

    let (count, text) = if let Some(config_sensor) = &config.sensor {
        let sensor_count: usize =
            config_sensor.devices.iter().map(|d| d.sensors.len()).sum();

        let mut by_device: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut names = BTreeSet::new();
        for (i, d) in config_sensor.devices.iter().enumerate() {
            by_device.entry(d.device.clone()).or_default().push(i);
            if !names.insert(d.name.clone()) {
                bail!("Duplicate sensor name: {}", d.name);
            }
        }

        let mut sensors_text = String::new();
        let mut sensor_id = 0;
        for d in &config_sensor.devices {
            for (sensor_type, &sensor_count) in d.sensors.iter() {
                let sensor = format!(
                    "{}_{}_{}",
                    d.device.to_ascii_uppercase(),
                    d.name.to_ascii_uppercase(),
                    sensor_type.to_ascii_uppercase()
                );
                writeln!(
                    &mut sensors_text,
                    "        #[allow(dead_code)]
        pub const NUM_{sensor}_SENSORS: usize = {sensor_count};"
                )
                .unwrap();
                if sensor_count == 1 {
                    writeln!(
                        &mut sensors_text,
                        "        #[allow(dead_code)]
        pub const {sensor}_SENSOR: SensorId = \
            // {}
            SensorId(NUM_I2C_SENSORS as u32 + {sensor_id});",
                        d.description
                    )
                    .unwrap();
                    sensor_id += 1;
                } else {
                    writeln!(
                        &mut sensors_text,
                        "        #[allow(dead_code)]
        pub const {sensor}_SENSORS: [SensorId; {sensor_count}] = ["
                    )
                    .unwrap();
                    for _ in 0..sensor_count {
                        writeln!(
                        &mut sensors_text,
                        "            SensorId(NUM_I2C_SENSORS as u32 + {sensor_id}),"
                    )
                        .unwrap();
                        sensor_id += 1;
                    }
                    writeln!(&mut sensors_text, "        ];").unwrap();
                }
            }
        }
        (sensor_count, sensors_text)
    } else {
        (0, String::new())
    };

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("sensor_config.rs");
    let mut file = std::fs::File::create(dest_path)?;
    writeln!(
        &mut file,
        r#"pub mod config {{
    #[allow(unused_imports)]
    use super::SensorId;

    // This is only included to determine the number of sensors
    include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

    pub mod other_sensors {{
        #[allow(unused_imports)]
        use super::SensorId;

        #[allow(unused_imports)]
        use super::NUM_I2C_SENSORS; // Used for offsetting

        #[allow(dead_code)]
        pub const NUM_SENSORS: usize = {count};
{text}
    }}

    pub use i2c_config::sensors as i2c_sensors;
    pub use i2c_sensors::NUM_SENSORS as NUM_I2C_SENSORS;
    pub use other_sensors::NUM_SENSORS as NUM_OTHER_SENSORS;

    // Here's what we actually care about:
    pub const NUM_SENSORS: usize = NUM_I2C_SENSORS + NUM_OTHER_SENSORS;
}}"#
    )
    .unwrap();
    Ok(())
}
