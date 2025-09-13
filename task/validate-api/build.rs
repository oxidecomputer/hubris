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

    #[cfg(feature = "fruid")]
    if let Err(e) = build_i2c::codegen(build_i2c::Disposition::Devices) {
        println!("cargo::error=failed to generate I2C devices: {e}");
        std::process::exit(1);
    }

    Ok(())
}

fn write_pub_device_descriptions() -> anyhow::Result<()> {
    use gateway_messages::SpComponent;
    let devices = build_i2c::device_descriptions().collect::<Vec<_>>();

    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path =
        std::path::Path::new(&out_dir).join("device_descriptions.rs");
    let file = std::fs::File::create(dest_path)?;
    let mut file = std::io::BufWriter::new(file);

    writeln!(
        file,
        "pub const MAX_ID_LENGTH: usize = {};",
        SpComponent::MAX_ID_LENGTH,
    )?;

    writeln!(
        file,
        "pub const DEVICES_CONST: [DeviceDescription; {}] = [",
        devices.len()
    )?;

    //
    // If a device in the TOML has no refdes, has the same refdes and suffix as
    // another device, or produces a refdes-and-suffix string that is longer
    // than the max component ID length, we will generate code that will not
    // compile, so these errors are all fatal. However, as we loop over devices,
    // we'll just log them and keep going, so that we can tell the user about
    // *all* the bad devices in the config file, rather than bailing out at the
    // first one. At the end, we return an error if there were any bad devices.
    // This way, you don't have to fix one issue and recompile in order to
    // discover the next error.
    //
    let mut missing_ids = 0;
    let mut duplicate_ids = 0;
    let mut ids_too_long = 0;
    #[cfg(feature = "fruid")]
    let mut bad_fruids = 0;
    //
    // The DEVICE_INDICES_BY_SORTED_ID array is used to look up indices by ID
    // using a binary search, so it must be sorted by ID. This map is used to
    // generate that array, so we use a BTreeMap here to ensure it's sorted by
    // key.
    //
    let mut id2idx = std::collections::BTreeMap::new();

    for (idx, dev) in devices.into_iter().enumerate() {
        writeln!(file, "    DeviceDescription {{")?;
        writeln!(file, "        device: {:?},", dev.device)?;
        writeln!(file, "        description: {:?},", dev.description)?;
        if let Some(id) = dev.device_id.as_ref() {
            let id = id.to_component_id();
            if let Ok(component) = SpComponent::try_from(id.as_ref()) {
                write!(file, "        id: {:?},", component.id)?;
                if id2idx.insert(component.id, idx).is_some() {
                    println!("cargo::error=duplicate device id {id:?}",);
                    duplicate_ids += 1;
                }
            } else {
                println!(
                    "cargo::error=device ID {id:?} exceeds max length ({}B)",
                    SpComponent::MAX_ID_LENGTH,
                );
                ids_too_long += 1;
            }
        } else {
            println!(
                "cargo::error=device {:?} ({:?}) hath no device ID (refdes)",
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
        #[cfg(feature = "fruid")]
        {
            let mode = match (dev.fruid, dev.device.as_ref()) {
                (Some(build_i2c::FruidMode::SingleBarcode), "at24csw080") => {
                    Some("FruidMode::At24Csw080Barcode")
                }
                (Some(build_i2c::FruidMode::NestedBarcode), "at24csw080") => {
                    Some("FruidMode::At24Csw080NestedBarcode")
                }
                (None, "tmp117") => Some("FruidMode::Tmp117"),
                (Some(build_i2c::FruidMode::Pmbus), _) => {
                    // TODO(elize): this should check that the device is a PMBus
                    // device and error if it isn't...
                    Some("FruidMode::Pmbus")
                }
                (Some(mode), device) => {
                    println!("cargo::error=FRUID mode {mode:?} not supported for {device}");
                    bad_fruids += 1;
                    None
                }
                (None, _) => None,
            };
            match (mode, dev.device_id.as_ref()) {
                (Some(mode), Some(id)) => {
                    let devname = format!(
                        "i2c_config::devices::{}_{} as fn(_) -> _",
                        &dev.device,
                        id.to_lower_ident()
                    );
                    writeln!(file, "        fruid: Some({mode}({devname})),")?;
                }
                (None, _) => {
                    writeln!(file, "        fruid: None,")?;
                }
                (Some(mode), None) => {
                    println!(
                        "cargo::error=devices with a FRUID mode must have \
                         refdes, but device {idx} (mode: {mode:?}) does not\
                        have one",
                    );
                    bad_fruids += 1;
                }
            }
        }
        writeln!(file, "    }},")?;
    }

    writeln!(file, "];")?;

    writeln!(
        file,
        "pub static DEVICES: [DeviceDescription; DEVICES_CONST.len()] = DEVICES_CONST;"
    )?;

    writeln!(
        file,
        "pub static DEVICE_INDICES_BY_SORTED_ID: [([u8; MAX_ID_LENGTH], usize); {}] = [",
        id2idx.len()
    )?;
    for (id, idx) in id2idx {
        writeln!(file, "    ({id:?}, {idx}),")?;
    }
    writeln!(file, "];")?;

    file.flush()?;

    anyhow::ensure!(missing_ids == 0, "{missing_ids} devices have no ID!");

    anyhow::ensure!(
        duplicate_ids == 0,
        "{duplicate_ids} duplicate device IDs!"
    );

    anyhow::ensure!(
        ids_too_long == 0,
        "{ids_too_long} device IDs exceeded max length ({}B)!",
        SpComponent::MAX_ID_LENGTH,
    );

    #[cfg(feature = "fruid")]
    anyhow::ensure!(
        bad_fruids == 0,
        "{bad_fruids} devices have invalid FRUID configs!"
    );

    Ok(())
}
