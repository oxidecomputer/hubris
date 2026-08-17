// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
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
    sensors: BTreeMap<build_i2c::Sensor, usize>,
    #[cfg_attr(not(feature = "component-id-lookup"), allow(dead_code))]
    refdes: Option<build_i2c::Refdes>,
}

fn main() -> Result<()> {
    idol::client::build_client_stub("../../idl/sensor.idol", "client_stub.rs")
        .map_err(|e| anyhow!("idol error: {e}"))?;

    let i2c_outputs = build_i2c::codegen(build_i2c::Disposition::Sensors)?;

    let i2c_sensors = i2c_outputs.sensors.expect(
        "i2c codegen should output `I2cSensorsDescription` if run with \
         `Disposition::Sensors`",
    );

    let config: GlobalConfig = build_util::config()?;

    let mut state = GeneratorState {
        num_other_sensors: 0,
        num_i2c_sensors: i2c_sensors.total_sensors,
        by_id: i2c_sensors.by_id,
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
                    "{}_{}_{sensor_type}",
                    d.device.to_ascii_uppercase(),
                    d.name.to_ascii_uppercase(),
                );
                writeln!(
                    &mut sensors_text,
                    "        #[allow(dead_code)]
        pub const NUM_{sensor}_SENSORS: usize = {sensor_count};"
                )
                .unwrap();

                if sensor_count == 1 {
                    let sensor_id = state.add_sensor(d, *sensor_type)?;
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
                        let sensor_id = state.add_sensor(d, *sensor_type)?;
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
        let mut max_len = 0;
        for sensor in &state.by_id {
            let cid = sensor
                .refdes
                .as_ref()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "we were asked to generate a sensor-ID-to-component-ID \
                        lookup table, but sensor ID {:?} (name: {:?}, type: \
                        {:?}) has no refdes",
                        sensor.id,
                        sensor.name.as_deref().unwrap_or("<no name>"),
                        sensor.kind,
                    )
                })?
                .to_component_id();
            max_len = max_len.max(cid.len());
            writeln!(
                &mut file,
                "        fixedstr::FixedStr::from_str(\"{cid}\"),",
            )
            .unwrap();
        }
        writeln!(&mut file, "    ];").unwrap();
        writeln!(
            &mut file,
            r#"pub const MAX_COMPONENT_ID_LEN: usize = {max_len};"#
        )
        .unwrap();
    }

    #[cfg(feature = "sensor-name-lookup")]
    {
        write!(
            &mut file,
            r#"
    pub(super) const SENSOR_ID_TO_NAME: [
        fixedstr::FixedStr<'static, MAX_SENSOR_NAME_LEN>;
        NUM_SENSORS
    ] = [
"#,
        )
        .unwrap();
        let mut max_len = 0;
        for sensor in &state.by_id {
            let Some(ref name) = sensor.name else {
                anyhow::bail!(
                    "we were asked to generate a sensor-name lookup table, but \
                     sensor {sensor:?} has no name"
                );
            };
            max_len = max_len.max(name.len());
            writeln!(
                &mut file,
                "        fixedstr::FixedStr::from_str(\"{name}\"),",
            )
            .unwrap();
        }
        writeln!(&mut file, "    ];").unwrap();
        writeln!(
            &mut file,
            r#"pub const MAX_SENSOR_NAME_LEN: usize = {max_len};"#
        )
        .unwrap();
    }

    writeln!(&mut file, "}}").unwrap();
    Ok(())
}

struct GeneratorState {
    num_i2c_sensors: usize,
    num_other_sensors: usize,
    by_id: iddqd::IdOrdMap<Arc<build_i2c::DeviceSensor>>,
}

impl GeneratorState {
    fn add_sensor(
        &mut self,
        d: &Sensor,
        sensor_type: build_i2c::Sensor,
    ) -> Result<usize> {
        let sensor_id = self.num_i2c_sensors + self.num_other_sensors;
        self.num_other_sensors += 1;

        #[cfg(feature = "component-id-lookup")]
        anyhow::ensure!(
            d.refdes.is_some(),
            "we were asked to generate a SensorId-to-component-id lookup \
             table, but non-I2C sensor {:?} (device: {:?}, ID: {sensor_id}) \
             has no refdes!",
            d.name,
            d.device,
        );

        let sensor = Arc::new(build_i2c::DeviceSensor {
            id: sensor_id,
            refdes: d.refdes.clone(),
            name: Some(d.name.clone()),
            kind: sensor_type,
        });
        self.by_id.insert_unique(sensor.clone()).map_err(|e| {
            anyhow::anyhow!(
                "duplicate sensor ID {sensor_id}\nwhile inserting {:?}\n\
                 duplicates: {:?}",
                e.new_item(),
                e.duplicates(),
            )
        })?;

        Ok(sensor_id)
    }
}
