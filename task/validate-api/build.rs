// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_i2c_types::PmbusCapabilities;
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
    //
    // The DEVICE_INDICES_BY_SORTED_ID array is used to look up indices by ID
    // using a binary search, so it must be sorted by ID. This map is used to
    // generate that array, so we use a BTreeMap here to ensure it's sorted by
    // key.
    //
    let mut id2idx = std::collections::BTreeMap::new();

    for (idx, dev) in devices.into_iter().enumerate() {
        let device_name = dev.device.as_str();
        let pmbus_capabilities = if dev.pmbus.is_some() {
            let Some(caps) =
                PMBUS_GENERATOR.iter().find_map(|&(name, generate)| {
                    (name == device_name).then(generate)
                })
            else {
                println!(
                    "cargo::error=unknown pmbus device: {device_name}, add an \
                     entry to PMBUS_GENERATOR in {} for PMBus status register \
                     and VPD support.",
                    file!(),
                );
                panic!("Unsupported pmbus device: {device_name}");
            };
            Some(caps)
        } else {
            None
        };
        let vpd = if pmbus_capabilities.is_some_and(|caps| {
            caps.supports_any(&PmbusCapabilities::ANY_VPD_REGS)
        }) {
            Some("Pmbus")
        } else if build_i2c::VPD_EEPROM_DEVICES.contains(&device_name) {
            let vpd_mode = match dev.eeprom_vpd.unwrap_or_default() {
                build_i2c::EepromVpd::SingleBarcode => "SingleBarcode",
                build_i2c::EepromVpd::SledFanTray => "SledFanTray",
            };
            Some(vpd_mode)
        } else if build_i2c::VPD_TMP11X_DEVICES.contains(&device_name) {
            Some("Tmp11x")
        } else {
            None
        };

        writeln!(file, "    DeviceDescription {{")?;
        writeln!(file, "        device: {:?},", dev.device)?;
        writeln!(file, "        description: {:?},", dev.description)?;
        if let Some(id) = dev.device_id.as_ref() {
            if id.len() <= SpComponent::MAX_ID_LENGTH {
                writeln!(file, "        id: \"{id}\",")?;
                if id2idx.insert(id.to_string(), idx).is_some() {
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
        match pmbus_capabilities {
            Some(caps) => writeln!(
                file,
                "        pmbus_capabilities: Some(drv_i2c_api::PmbusCapabilities(0x{:08x})),",
                caps.0,
            )?,
            None => writeln!(file, "        pmbus_capabilities: None,")?,
        }
        match vpd {
            Some(vpd) => writeln!(file, "        vpd: Some(VpdKind::{vpd}),")?,
            None => writeln!(file, "        vpd: None,")?,
        }
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
        "pub static DEVICE_INDICES_BY_SORTED_ID: [(&str, usize); {}] = [",
        id2idx.len()
    )?;
    for (id, idx) in id2idx {
        writeln!(file, "    (\"{id}\", {idx}),")?;
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

    Ok(())
}

/// Look at the `pmbus` crate metadata to see if a specific command is "Illegal"
/// and set the capability bit if not.
macro_rules! set_if_pmbus_read_illegal {
    ($out:ident, $module:ident, $cmd:ident) => {{
        use pmbus::{Command, Operation};
        if pmbus::commands::$module::CommandCode::$cmd.read_op()
            != Operation::Illegal
        {
            $out |= PmbusCapabilities::$cmd.0;
        }
    }};
}

/// Calculates the supported PMBus status and VPD registers for a device.
///
/// The pmbus functions are not const, so generate a closure instead.
macro_rules! pmbus_generator {
    ($name:literal, $module:ident) => {
        ($name, || {
            let mut out = 0u32;
            set_if_pmbus_read_illegal!(out, $module, STATUS_WORD);
            set_if_pmbus_read_illegal!(out, $module, STATUS_VOUT);
            set_if_pmbus_read_illegal!(out, $module, STATUS_IOUT);
            set_if_pmbus_read_illegal!(out, $module, STATUS_TEMPERATURE);
            set_if_pmbus_read_illegal!(out, $module, STATUS_CML);
            set_if_pmbus_read_illegal!(out, $module, STATUS_OTHER);
            set_if_pmbus_read_illegal!(out, $module, STATUS_INPUT);
            set_if_pmbus_read_illegal!(out, $module, STATUS_MFR_SPECIFIC);
            set_if_pmbus_read_illegal!(out, $module, STATUS_FANS_1_2);
            set_if_pmbus_read_illegal!(out, $module, STATUS_FANS_3_4);
            // VPD bits
            set_if_pmbus_read_illegal!(out, $module, MFR_ID);
            set_if_pmbus_read_illegal!(out, $module, MFR_MODEL);
            set_if_pmbus_read_illegal!(out, $module, MFR_REVISION);
            set_if_pmbus_read_illegal!(out, $module, MFR_SERIAL);
            set_if_pmbus_read_illegal!(out, $module, MFR_LOCATION);
            set_if_pmbus_read_illegal!(out, $module, MFR_DATE);
            set_if_pmbus_read_illegal!(out, $module, IC_DEVICE_ID);
            set_if_pmbus_read_illegal!(out, $module, IC_DEVICE_REV);
            PmbusCapabilities(out)
        })
    };
}

type PmbusDeviceRow = (&'static str, fn() -> PmbusCapabilities);

// Before you add a pmbus device to this list, you should make sure that you
// have reviewed the pmbus crate to make sure that any unsupported status
// registers are marked as illegal, similar to oxidecomputer/pmbus#35.
//
// Failure to do so could cause CML or OTHER error bits to be set. Just adding
// the device to this list (without accurate `pmbus` crate information) will
// likely make the compilation succeed, but should not be done for production
// devices where this may trigger runtime CML errors.
const PMBUS_GENERATOR: &[PmbusDeviceRow] = &[
    pmbus_generator!("adm127x", adm127x),
    pmbus_generator!("bmr491", bmr491),
    pmbus_generator!("isl68224", isl68224),
    pmbus_generator!("lm5066", lm5066),
    pmbus_generator!("lm5066i", lm5066i),
    pmbus_generator!("mwocp67", mwocp67),
    pmbus_generator!("mwocp68", mwocp68),
    pmbus_generator!("raa229618", raa229618),
    pmbus_generator!("raa229620a", raa229620a),
    pmbus_generator!("tps546b24a", tps546b24a),
];
