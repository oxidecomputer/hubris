// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Err(e) = build_i2c::codegen(build_i2c::Disposition::Devices) {
        println!("cargo::error=failed to generate I2C devices: {e}");
        std::process::exit(1);
    }

    write_pub_device_descriptions()?;

    idol::client::build_client_stub(
        "../../idl/validate.idol",
        "client_stub.rs",
    )?;
    Ok(())
}

fn write_pub_device_descriptions() -> anyhow::Result<()> {
    use gateway_messages::SpComponent;
    let devices = build_i2c::device_descriptions().collect::<Vec<_>>();

    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path =
        std::path::Path::new(&out_dir).join("device_descriptions.rs");
    let file = std::fs::File::create(&dest_path)?;
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
    //
    // The DEVICE_INDICES_BY_SORTED_ID array is used to look up indices by ID
    // using a binary search, so it must be sorted by ID. This map is used to
    // generate that array, so we use a BTreeMap here to ensure it's sorted by
    // key.
    //
    let mut id2idx = std::collections::BTreeMap::new();
    let mut pmbus_rail_names = std::collections::BTreeSet::new();

    for (idx, dev) in devices.iter().cloned().enumerate() {
        let is_pmbus = dev.is_pmbus();
        writeln!(file, "    DeviceDescription {{")?;
        writeln!(file, "        device: {:?},", dev.device)?;
        writeln!(file, "        description: {:?},", dev.description)?;
        if let Some(ref id) = dev.device_id {
            if let Ok(component) = SpComponent::try_from(id.as_ref()) {
                writeln!(file, "        id: {:?},", component.id)?;
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

            if let Some(ref pmbus) = dev.pmbus {
                for rail in pmbus.rails.iter() {
                    // Returns "is unique", unlike `BTreeMap::insert().is_some()`!
                    if !pmbus_rail_names.insert(rail.name.clone()) {
                        panic!("cargo::warn=dupe: {:?}, {:?}", rail.name, dev);
                    }
                }
            }
        } else {
            println!(
                "cargo::error=device {:?} ({:?}) hath no device ID (refdes)",
                dev.device, dev.description
            );
            missing_ids += 1;
        };
        writeln!(file, "        is_pmbus: {is_pmbus:?},")?;
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
        "pub static DEVICE_INDICES_BY_SORTED_ID: [([u8; MAX_ID_LENGTH], usize); {}] = [",
        id2idx.len()
    )?;
    for (id, idx) in id2idx {
        writeln!(file, "    ({id:?}, {idx}),")?;
    }
    writeln!(file, "];")?;

    let max_len = pmbus_rail_names.iter().map(String::len).max();
    if let Some(_len) = max_len {

        // TODO: do we need fixed-length bstrings? If we want to binary search, we probably want them to
        // be truncated to some maximum length, so we either need to define the max length at the
        // protocol level so mgs knows how long of a string to send us, or we could instead trim the
        // trailing nulls and search by that instead.
        //
        // writeln!(file, "pub const MAX_PMBUS_RAIL_NAME: usize = {len};")?;
        // writeln!(file, "pub const PMBUS_RAIL_TO_I2C_DEVICE_MAP: [([u8; MAX_PMBUS_RAIL_NAME], fn(TaskId) -> (drv_i2c_api::I2cDevice, u8)); {}] = [", pmbus_rail_names.len())?;
        // for rail in pmbus_rail_names.iter() {
        //     write!(file, "    (*b\"{rail}")?;
        //     for _ in 0..(len.checked_sub(rail.len()).unwrap()) {
        //         write!(file, "\\0")?;
        //     }
        //     write!(file, "\", ")?;
        //     write!(file, "crate::i2c_config::pmbus::{}", rail.to_lowercase())?;
        //     writeln!(file, "),")?;
        // }
        // writeln!(file, "];")?;

        // Assuming we are going the trimmed route...
        writeln!(file)?;
        writeln!(file, "pub const PMBUS_RAIL_TO_I2C_DEVICE_MAP: [(&[u8], fn(TaskId) -> (drv_i2c_api::I2cDevice, u8)); {}] = [", pmbus_rail_names.len())?;
        for rail in pmbus_rail_names.iter() {
            write!(file, "    (b\"{rail}\", ")?;
            // TODO: Do we need a fancier rail -> func conversion than "to_lowercase"? Probably should check
            // what build_i2c does for generating the names and match that.
            write!(file, "crate::i2c_config::pmbus::{}", rail.to_lowercase())?;
            writeln!(file, "),")?;
        }
        writeln!(file, "];")?;
    }

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


    // panic!("{}", dest_path.display());
    // panic!("{max_len:?} {:#?}", pmbus_rail_names);

    Ok(())
}
