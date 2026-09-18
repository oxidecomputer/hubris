// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{Result, anyhow};
use build_i2c::{CodegenSettings, Disposition, Section};

fn main() -> Result<()> {
    idol::client::build_client_stub("../../idl/sensor.idol", "client_stub.rs")
        .map_err(|e| anyhow!("idol error: {e}"))?;

    //
    // Generate the I2C devices and sensors, plus the non-I2C sensors from
    // `[config.sensor]` (which share the sensor ID space with the I2C
    // sensors) and, depending on our features, lookup tables over all of
    // them. See the `config` module in `lib.rs` for how these are exposed.
    //
    let mut settings: CodegenSettings = Disposition::Sensors.into();
    settings.sections.push(Section::OtherSensors);

    if cfg!(feature = "component-id-lookup") {
        settings.sections.push(Section::SensorIdToComponentId);
    }

    if cfg!(feature = "sensor-name-lookup") {
        settings.sections.push(Section::SensorIdToName);
    }

    build_i2c::codegen_to_file(settings)?;

    Ok(())
}
