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
    #[cfg_attr(not(feature = "component-id-lookup"), allow(dead_code))]
    refdes: Option<build_i2c::Refdes>,
}

fn main() -> Result<()> {
    idol::client::build_client_stub("../../idl/sensor.idol", "client_stub.rs")
        .map_err(|e| anyhow!("idol error: {e}"))?;

    let i2c_outputs = build_i2c::codegen(build_i2c::Disposition::Sensors)?;

    #[cfg(feature = "component-id-lookup")]
    let component_ids_by_id = i2c_outputs.component_ids_by_sensor_id.expect(
        "component IDs by sensor ID map should be generated if \
         `build-i2c/component-id` feature is enabled",
    );
    let num_i2c_sensors = i2c_outputs.num_i2c_sensors.expect(
        "i2c codegen should output `num_i2c_sensors` if run with \
         `Disposition::Sensors`",
    );

    let config: GlobalConfig = build_util::config()?;

    let mut state = GeneratorState {
        num_other_sensors: 0,
        num_i2c_sensors,
        #[cfg(feature = "component-id-lookup")]
        component_ids_by_id,
        #[cfg(feature = "component-id-lookup")]
        max_component_id_len: 0,
    };
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
                    let sensor_id = state.add_sensor(d)?;
                    writeln!(
                        &mut sensors_text,
                        "        #[allow(dead_code)]
        pub const {sensor}_SENSOR: SensorId = \
            // {}
            SensorId({sensor_id});",
                        d.description
                    )
                    .unwrap();
                } else {
                    writeln!(
                        &mut sensors_text,
                        "        #[allow(dead_code)]
        pub const {sensor}_SENSORS: [SensorId; {sensor_count}] = ["
                    )
                    .unwrap();
                    for _ in 0..sensor_count {
                        let sensor_id = state.add_sensor(d)?;
                        writeln!(
                            &mut sensors_text,
                            "            SensorId({sensor_id}),"
                        )
                        .unwrap();
                    }
                    writeln!(&mut sensors_text, "        ];").unwrap();
                }
            }
        }

        #[cfg(feature = "component-id-lookup")]
        writeln!(
            &mut sensors_text,
            "        pub const MAX_COMPONENT_ID_LEN: usize = {};",
            state.max_component_id_len
        )
        .unwrap();
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

    include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

    pub mod other_sensors {{
        #[allow(unused_imports)]
        use super::SensorId;

        #[allow(dead_code)]
        pub const NUM_SENSORS: usize = {count};
{text}
    }}

    pub use i2c_config::sensors as i2c_sensors;
    pub use i2c_sensors::NUM_SENSORS as NUM_I2C_SENSORS;
    pub use other_sensors::NUM_SENSORS as NUM_OTHER_SENSORS;

    pub const NUM_SENSORS: usize = NUM_I2C_SENSORS + NUM_OTHER_SENSORS;
"#
    )
    .unwrap();

    #[cfg(feature = "component-id-lookup")]
    {
        writeln!(&mut file,
            r#"pub const MAX_COMPONENT_ID_LEN: usize = if other_sensors::MAX_COMPONENT_ID_LEN > i2c_config::MAX_COMPONENT_ID_LEN {{
                other_sensors::MAX_COMPONENT_ID_LEN
            }} else {{
                i2c_config::MAX_COMPONENT_ID_LEN
            }};"#
        )
        .unwrap();
        write!(
            &mut file,
            r#"
    pub(super) const SENSOR_ID_TO_COMPONENT_ID: [
        fixedstr::FixedStr<'static, MAX_COMPONENT_ID_LEN>;
        NUM_SENSORS
    ] = [
"#,
        )
        .unwrap();
        for (_, cid) in state.component_ids_by_id {
            writeln!(
                &mut file,
                "        fixedstr::FixedStr::from_str(\"{cid}\"),",
            )
            .unwrap();
        }
        writeln!(&mut file, "    ];").unwrap();
    }

    writeln!(&mut file, "}}").unwrap();
    Ok(())
}

struct GeneratorState {
    num_i2c_sensors: usize,
    num_other_sensors: usize,
    #[cfg(feature = "component-id-lookup")]
    component_ids_by_id: BTreeMap<usize, String>,
    #[cfg(feature = "component-id-lookup")]
    max_component_id_len: usize,
}

impl GeneratorState {
    fn add_sensor(&mut self, _d: &Sensor) -> Result<usize> {
        let sensor_id = self.num_i2c_sensors + self.num_other_sensors;
        self.num_other_sensors += 1;
        #[cfg(feature = "component-id-lookup")]
        {
            let d = _d;
            let Some(ref refdes) = d.refdes else {
                anyhow::bail!(
                    "we were asked to generate sensor component IDs, but \
                     sensor {} has no refdes",
                    d.name,
                )
            };
            let component_id = refdes.to_component_id();
            self.max_component_id_len =
                self.max_component_id_len.max(component_id.len());
            if let Some(prev) =
                self.component_ids_by_id.insert(sensor_id, component_id)
            {
                anyhow::bail!(
                    "duplicate sensor ID {sensor_id} for {} \
                     (previous entry had refdes {prev})",
                    d.name,
                );
            }
        }

        Ok(sensor_id)
    }
}
