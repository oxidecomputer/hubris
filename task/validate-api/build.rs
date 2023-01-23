// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_pub_device_descriptions()?;

    idol::client::build_client_stub(
        "../../idl/validate.idol",
        "client_stub.rs",
    )?;
    Ok(())
}

fn write_pub_device_descriptions(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let devices = build_i2c::device_descriptions().collect::<Vec<_>>();

    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path =
        std::path::Path::new(&out_dir).join("device_descriptions.rs");
    let file = std::fs::File::create(dest_path)?;
    let mut file = std::io::BufWriter::new(file);

    writeln!(
        file,
        "pub const DEVICES: [DeviceDescription; {}] = [",
        devices.len()
    )?;

    for dev in devices {
        writeln!(file, "    DeviceDescription {{")?;
        writeln!(file, "        device: {:?},", dev.device)?;
        writeln!(file, "        description: {:?},", dev.description)?;
        writeln!(file, "        sensors: &[")?;
        for s in dev.sensors {
            writeln!(file, "            SensorDescription {{")?;
            writeln!(file, "                name: {:?},", s.name)?;
            writeln!(file, "                kind: Sensor::{:?},", s.kind)?;
            writeln!(file, "                id: SensorId({}),", s.id)?;
            writeln!(file, "            }},")?;
        }
        writeln!(file, "        ],")?;
        writeln!(file, "    }},")?;
    }

    writeln!(file, "];")?;
    file.flush()?;

    Ok(())
}
