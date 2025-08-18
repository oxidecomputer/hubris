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

fn write_pub_device_descriptions() -> anyhow::Result<()> {
    let devices = build_i2c::device_descriptions().collect::<Vec<_>>();

    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path =
        std::path::Path::new(&out_dir).join("device_descriptions.rs");
    let file = std::fs::File::create(dest_path)?;
    let mut file = std::io::BufWriter::new(file);

    writeln!(
        file,
        "pub const DEVICES_CONST: [DeviceDescription; {}] = [",
        devices.len()
    )?;

    let mut missing_ids = 0;
    let mut duplicate_ids = 0;
    let mut id2idx = std::collections::BTreeMap::new();

    for (idx, dev) in devices.into_iter().enumerate() {
        writeln!(file, "    DeviceDescription {{")?;
        writeln!(file, "        device: {:?},", dev.device)?;
        writeln!(file, "        description: {:?},", dev.description)?;
        if let Some(id) = dev.refdes.or_else(|| dev.name) {
            writeln!(file, "        id: {id:?},")?;
            if id2idx.insert(id.clone(), idx).is_some() {
                println!("cargo::error=duplicate device id {id:?}",);
                duplicate_ids += 1;
            }
        } else {
            println!(
                "cargo::error=device {:?} ({:?}) missing both name and refdes",
                dev.device, dev.description
            );
            missing_ids += 1;
        };
        writeln!(file, "        sensors: &[")?;
        for s in dev.sensors {
            writeln!(file, "            SensorDescription {{")?;
            writeln!(file, "                name: {:?},", s.name)?;
            writeln!(file, "                kind: Sensor::{:?},", s.kind)?;
            writeln!(file, "                id: SensorId::new({}),", s.id)?;
            writeln!(file, "            }},")?;
        }
        writeln!(file, "        ],")?;
        writeln!(file, "    }},")?;
    }

    writeln!(file, "];")?;

    writeln!(
        file,
        "pub static DEVICES: [DeviceDescription; DEVICES_CONST.len()] = DEVICES_CONST;"
    )?;

    writeln!(
        file,
        "pub static DEVICE_INDICES_BY_ID: [(&'static str, usize); {}] = [",
        id2idx.len()
    )?;
    for (id, idx) in id2idx {
        writeln!(file, "    ({id:?}, {idx}")?;
    }
    writeln!(file, "];")?;

    file.flush()?;

    anyhow::ensure!(
        missing_ids == 0,
        "{missing_ids} devices had neither name nor refdes"
    );

    anyhow::ensure!(
        duplicate_ids == 0,
        "{duplicate_ids} duplicate device IDs!"
    );

    Ok(())
}
