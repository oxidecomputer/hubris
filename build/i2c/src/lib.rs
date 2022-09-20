// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{bail, Context, Result};
use convert_case::{Case, Casing};
use indexmap::IndexMap;
use multimap::MultiMap;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fmt::Write;
use std::fs::File;
use std::path::Path;

//
// Our definition of the `Config` type.  We share this type with all other
// build-specific types; we must not set `deny_unknown_fields` here.
//
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Config {
    i2c: I2cConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct I2cConfig {
    controllers: Vec<I2cController>,
    devices: Option<Vec<I2cDevice>>,
}

//
// Note that [`ports`] is a `BTreeMap` (rather than, say, an `IndexMap`).
// This is load-bearing!  It is essential that deserialization of our
// application TOML have the same ordering for the ports, as the index is used
// by the debugger to denote a desired port.  One might think that an
// `IndexMap` would assure this, but because our configuration is reserialized
// as part of the build process (with the re-serialized TOML being stuffed
// into an environment variable), and because TOML is not stable with respect
// to the ordering of a table (both in terms of the specification -- see e.g.
// https://github.com/toml-lang/toml/issues/162 -- and in terms of the toml-rs
// implementation which, by default, uses a `BTreeMap` rather than an
// `IndexMap` for tables), we must be sure to impose our own (absolute)
// ordering.
//
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct I2cController {
    controller: u8,
    ports: BTreeMap<String, I2cPort>,
    #[serde(default)]
    target: bool,
}

//
// Unfortunately, the toml-rs parsing of enums isn't quite right (see
// https://github.com/alexcrichton/toml-rs/issues/390 for details).  As a
// result, we currently flatten what really should be enums around topology
// (i.e., [`controller`]/[`port`] vs. [`bus`]) and device class parameters
// (i.e., [`pmbus`]) into optional fields in [`I2cDevice`].  This makes it
// easier to accidentally create invalid entries (e.g., a device that has both
// a controller *and* a named bus), so the validation code should go to
// additional lengths to assure that these mistakes are caught in compilation.
//
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[allow(dead_code)]
struct I2cDevice {
    /// device part name
    device: String,

    /// device name
    name: Option<String>,

    /// I2C controller, if bus not named
    controller: Option<u8>,

    /// I2C bus name, if controller not specified
    bus: Option<String>,

    /// I2C port, if required
    port: Option<String>,

    /// I2C address
    address: u8,

    /// I2C mux, if any
    mux: Option<u8>,

    /// I2C segment, if any
    segment: Option<u8>,

    /// description of device
    description: String,

    /// reference designator, if any
    refdes: Option<String>,

    /// PMBus information, if any
    pmbus: Option<I2cPmbus>,

    /// sensor information, if any
    sensors: Option<I2cSensors>,

    /// device is removable
    #[serde(default)]
    removable: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct I2cPort {
    name: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
    pins: Vec<I2cPinSet>,
    #[serde(default)]
    muxes: Vec<I2cMux>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cPinSet {
    gpio_port: Option<String>,
    pins: Vec<u8>,
    af: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct I2cMux {
    driver: String,
    address: u8,
    enable: Option<I2cPinSet>,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct I2cPmbus {
    rails: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[allow(dead_code)]
struct I2cSensors {
    #[serde(default)]
    temperature: usize,

    #[serde(default)]
    power: usize,

    #[serde(default)]
    current: usize,

    #[serde(default)]
    voltage: usize,

    #[serde(default)]
    speed: usize,

    names: Option<Vec<String>>,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Disposition {
    /// controller is an initiator
    Initiator,

    /// controller is a target
    Target,

    /// devices are used (i.e., controller is not used), but not as sensors
    Devices,

    /// devices are used, with some used as sensors
    Sensors,

    /// devices are used, but only as validation
    Validation,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum Sensor {
    Temperature,
    Power,
    Current,
    Voltage,
    Speed,
}

impl std::fmt::Display for Sensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Sensor::Temperature => "TEMPERATURE",
                Sensor::Power => "POWER",
                Sensor::Current => "CURRENT",
                Sensor::Voltage => "VOLTAGE",
                Sensor::Speed => "SPEED",
            }
        )
    }
}

struct ConfigGenerator {
    /// output that we're building
    output: String,

    /// disposition of this configuration: target v. initiator v. devices
    disposition: Disposition,

    /// all controllers
    controllers: Vec<I2cController>,

    /// all devices
    devices: Vec<I2cDevice>,

    /// hash bus name to controller/port index pair
    buses: HashMap<String, (u8, usize)>,

    /// hash controller/port pair to port index
    ports: IndexMap<(u8, String), usize>,

    /// hash of controllers to single port indices
    singletons: HashMap<u8, usize>,
}

impl ConfigGenerator {
    fn new(disposition: Disposition) -> Self {
        let i2c = match build_util::config::<Config>() {
            Ok(config) => config.i2c,
            Err(err) => {
                panic!("malformed config.i2c: {:?}", err);
            }
        };

        let mut controllers = vec![];
        let mut buses = HashMap::new();
        let mut ports = IndexMap::new();
        let mut singletons = HashMap::new();

        for c in i2c.controllers {
            //
            // We always insert our buses (even for controllers that don't
            // match our dispostion) to assure that devices can always find
            // their bus.
            //
            for (index, (p, port)) in c.ports.iter().enumerate() {
                if let Some(name) = &port.name {
                    if buses
                        .insert(name.clone(), (c.controller, index))
                        .is_some()
                    {
                        panic!("i2c bus {} appears twice", name);
                    }
                }

                if c.ports.len() == 1 {
                    singletons.insert(c.controller, index);
                }

                ports.insert((c.controller, p.clone()), index);
            }

            if c.target != (disposition == Disposition::Target) {
                continue;
            }

            controllers.push(c);
        }

        if let Some(devices) = &i2c.devices {
            for d in devices {
                match (d.controller, d.bus.as_ref()) {
                    (None, None) => {
                        panic!(
                            "device {} at address {:#x} must have \
                            a bus or controller",
                            d.device, d.address
                        );
                    }
                    (Some(_), Some(_)) => {
                        panic!(
                            "device {} at address {:#x} has both \
                            a bus and a controller",
                            d.device, d.address
                        );
                    }
                    (_, Some(bus)) if buses.get(bus).is_none() => {
                        panic!(
                            "device {} at address {:#x} specifies \
                            unknown bus \"{}\"",
                            d.device, d.address, bus
                        );
                    }
                    (_, _) => {}
                }
            }
        }

        Self {
            output: String::new(),
            devices: i2c.devices.unwrap_or_default(),
            disposition,
            controllers,
            buses,
            ports,
            singletons,
        }
    }

    pub fn ncontrollers(&self) -> usize {
        self.controllers.len()
    }

    pub fn generate_header(&mut self) -> Result<()> {
        writeln!(&mut self.output, "mod i2c_config {{")?;
        Ok(())
    }

    pub fn generate_footer(&mut self) -> Result<()> {
        writeln!(&mut self.output, "}}")?;
        Ok(())
    }

    pub fn generate_controllers(&mut self) -> Result<()> {
        let mut s = &mut self.output;

        match self.disposition {
            Disposition::Initiator | Disposition::Target => {}

            _ => {
                panic!("illegal disposition for controller generation");
            }
        }

        writeln!(
            &mut s,
            r##"
    #[allow(dead_code)]
    pub const NCONTROLLERS: usize = {ncontrollers};

    use drv_stm32xx_i2c::I2cController;

    pub fn controllers() -> [I2cController<'static>; NCONTROLLERS] {{"##,
            ncontrollers = self.controllers.len()
        )?;

        if !self.controllers.is_empty() {
            writeln!(
                &mut s,
                r##"
        use drv_stm32xx_sys_api::Peripheral;
        use drv_i2c_api::Controller;

        #[cfg(feature = "h743")]
        use stm32h7::stm32h743 as device;

        #[cfg(feature = "h753")]
        use stm32h7::stm32h753 as device;

        #[cfg(feature = "h7b3")]
        use stm32h7::stm32h7b3 as device;

        #[cfg(feature = "g031")]
        use stm32g0::stm32g031 as device;"##
            )?;
        }

        write!(
            &mut s,
            r##"
        ["##
        )?;

        for c in &self.controllers {
            write!(
                &mut s,
                r##"
            I2cController {{
                controller: Controller::I2C{controller},
                peripheral: Peripheral::I2c{controller},
                notification: (1 << {shift}),
                registers: unsafe {{ &*device::I2C{controller}::ptr() }},
            }},"##,
                shift = c.controller - 1,
                controller = c.controller,
            )?;
        }

        writeln!(
            &mut s,
            r##"
        ]
    }}"##
        )?;

        Ok(())
    }

    pub fn generate_pins(&mut self) -> Result<()> {
        let mut s = &mut self.output;
        let mut len = 0;

        match self.disposition {
            Disposition::Initiator | Disposition::Target => {}

            _ => {
                panic!("illegal disposition for pin generation");
            }
        }

        for c in &self.controllers {
            for port in c.ports.values() {
                len += port.pins.len();
            }
        }

        writeln!(
            &mut s,
            r##"
    use drv_stm32xx_i2c::I2cPin;

    pub fn pins() -> [I2cPin; {}] {{"##,
            len
        )?;

        if len > 0 {
            writeln!(
                &mut s,
                r##"
        use drv_i2c_api::{{Controller, PortIndex}};
        use drv_stm32xx_sys_api::{{self as gpio_api, Alternate}};"##
            )?;
        }

        write!(
            &mut s,
            r##"
        ["##
        )?;

        for c in &self.controllers {
            for (index, (p, port)) in c.ports.iter().enumerate() {
                for pin in &port.pins {
                    let mut pinstr = String::new();
                    write!(&mut pinstr, "pin({})", pin.pins[0])?;

                    for i in 1..pin.pins.len() {
                        write!(&mut pinstr, ".and_pin({})", pin.pins[i])?;
                    }

                    write!(
                        &mut s,
                        r##"
            I2cPin {{
                controller: Controller::I2C{controller},
                port: PortIndex({i2c_port}),
                gpio_pins: gpio_api::Port::{gpio_port}.{pinstr},
                function: Alternate::AF{af},
            }},"##,
                        controller = c.controller,
                        i2c_port = index,
                        gpio_port = match pin.gpio_port {
                            Some(ref port) => port,
                            None => p,
                        },
                        pinstr = pinstr,
                        af = pin.af
                    )?;
                }
            }
        }

        writeln!(
            &mut s,
            r##"
        ]
    }}"##
        )?;

        Ok(())
    }

    pub fn generate_muxes(&mut self) -> Result<()> {
        if self.disposition == Disposition::Target {
            panic!("cannot generate muxes when configured as target");
        }

        let mut s = &mut self.output;
        let mut nmuxedbuses = 0;
        let mut len = 0;

        for c in &self.controllers {
            for port in c.ports.values() {
                if !port.muxes.is_empty() {
                    nmuxedbuses += 1;
                }

                len += port.muxes.len();
            }
        }

        write!(
            &mut s,
            r##"
    #[allow(dead_code)]
    pub const NMUXEDBUSES: usize = {nmuxedbuses};

    use drv_stm32xx_i2c::I2cMux;

    pub fn muxes() -> [I2cMux<'static>; {}] {{"##,
            len
        )?;

        if len > 0 {
            writeln!(
                &mut s,
                r##"
        use drv_i2c_api::{{Controller, PortIndex, Mux}};

        #[allow(unused_imports)]
        use drv_stm32xx_sys_api::{{self as gpio_api, Alternate}};"##
            )?;
        }

        write!(
            &mut s,
            r##"
        ["##
        )?;

        for c in &self.controllers {
            for (index, (p, port)) in c.ports.iter().enumerate() {
                for (mindex, mux) in port.muxes.iter().enumerate() {
                    let enablestr = if let Some(enable) = &mux.enable {
                        let mut enablestr = String::new();
                        write!(
                            &mut enablestr,
                            r##"Some(I2cPin {{
                    controller: Controller::I2C{controller},
                    port: PortIndex({port}),
                    gpio_pins: gpio_api::Port::{gpio_port}.pin({gpio_pin}),
                    function: Alternate::AF{af},
                }})"##,
                            controller = c.controller,
                            port = index,
                            gpio_port = match enable.gpio_port {
                                Some(ref port) => port,
                                None => bail!(
                                    "missing pin port on mux enable \
                                    on I2C{}, port {}, mux {}",
                                    c.controller,
                                    p,
                                    mindex + 1
                                ),
                            },
                            gpio_pin = enable.pins[0],
                            af = enable.af
                        )?;
                        enablestr
                    } else {
                        "None".to_string()
                    };

                    let driver_struct = format!(
                        "{}{}",
                        mux.driver[..1].to_uppercase(),
                        &mux.driver[1..]
                    );

                    write!(
                        &mut s,
                        r##"
            I2cMux {{
                controller: Controller::I2C{controller},
                port: PortIndex({i2c_port}),
                id: Mux::M{mindex},
                driver: &drv_stm32xx_i2c::{driver}::{driver_struct},
                enable: {enable},
                address: {address:#x},
            }},"##,
                        controller = c.controller,
                        i2c_port = index,
                        mindex = mindex + 1,
                        driver = mux.driver,
                        driver_struct = driver_struct,
                        enable = enablestr,
                        address = mux.address,
                    )?;
                }
            }
        }

        writeln!(
            &mut s,
            r##"
        ]
    }}"##
        )?;

        Ok(())
    }

    fn generate_device(&self, d: &I2cDevice, indent: usize) -> String {
        let controller = match &d.bus {
            Some(bus) => self.buses.get(bus).unwrap().0,
            None => d.controller.unwrap(),
        };

        let indent = format!("{:indent$}", "", indent = indent);

        let port = match (&d.bus, &d.port) {
            (Some(_), Some(_)) => {
                panic!("device {} has both port and bus", d.device);
            }

            (Some(bus), None) => match self.buses.get(bus) {
                Some((_, port)) => port,
                None => {
                    panic!("device {} has invalid bus", d.device);
                }
            },

            (None, Some(port)) => {
                match self.ports.get(&(controller, port.to_string())) {
                    None => {
                        panic!("device {} has invalid port", d.device);
                    }
                    Some(port) => port,
                }
            }

            //
            // We allow ports to be unspecified if the specified
            // controller has only a single port; check the singletons.
            //
            (None, None) => match self.singletons.get(&controller) {
                Some(port) => port,
                None => {
                    panic!("device {} has ambiguous port", d.device)
                }
            },
        };

        let segment = match (d.mux, d.segment) {
            (Some(mux), Some(segment)) => {
                format!(
                    "Some((drv_i2c_api::Mux::M{}, drv_i2c_api::Segment::S{}))",
                    mux, segment
                )
            }
            (None, None) => "None".to_owned(),
            (Some(_), None) => {
                panic!("device {} specifies a mux but no segment", d.device)
            }
            (None, Some(_)) => {
                panic!("device {} specifies a segment but no mux", d.device)
            }
        };

        format!(
            r##"
{indent}// {description}
{indent}I2cDevice::new(task,
{indent}    Controller::I2C{controller},
{indent}    PortIndex({port}),
{indent}    {segment},
{indent}    {address:#x}
{indent})"##,
            description = d.description,
            controller = controller,
            port = port,
            segment = segment,
            address = d.address,
            indent = indent,
        )
    }

    pub fn generate_devices(&mut self) -> Result<()> {
        //
        // Throw all devices into a MultiMap based on device.
        //
        let mut bydevice = MultiMap::new();
        let mut byname = HashMap::new();
        let mut bybus = MultiMap::new();

        for d in &self.devices {
            bydevice.insert(&d.device, d);

            if let Some(bus) = &d.bus {
                bybus.insert((&d.device, bus), d);
            }

            if let Some(name) = &d.name {
                if byname.insert((&d.device, name), d).is_some() {
                    panic!("duplicate name {} for device {}", name, d.device)
                }
            }
        }

        write!(
            &mut self.output,
            r##"
    pub mod devices {{
        use drv_i2c_api::{{I2cDevice, Controller, PortIndex}};
        use userlib::TaskId;
"##
        )?;

        for (device, devices) in bydevice.iter_all() {
            write!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub fn {}(task: TaskId) -> [I2cDevice; {}] {{
            ["##,
                device,
                devices.len()
            )?;

            for d in devices {
                let out = self.generate_device(d, 16);
                write!(&mut self.output, "{},", out)?;
            }

            writeln!(
                &mut self.output,
                r##"
            ]
        }}"##
            )?;
        }

        for ((device, bus), devices) in bybus.iter_all() {
            write!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub fn {}_{}(task: TaskId) -> [I2cDevice; {}] {{
            ["##,
                device,
                bus,
                devices.len()
            )?;

            for d in devices {
                let out = self.generate_device(d, 16);
                write!(&mut self.output, "{},", out)?;
            }
            writeln!(
                &mut self.output,
                r##"
            ]
        }}"##
            )?;
        }

        for ((device, name), d) in &byname {
            write!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub fn {}_{}(task: TaskId) -> I2cDevice {{"##,
                device,
                name.to_lowercase()
            )?;

            let out = self.generate_device(d, 16);
            write!(&mut self.output, "{}", out)?;

            writeln!(
                &mut self.output,
                r##"
        }}"##
            )?;
        }

        writeln!(&mut self.output, "    }}")?;
        Ok(())
    }

    pub fn generate_validation(&mut self) -> Result<()> {
        //
        // Lord, have mercy: we are going to find the crate containing i2c
        // devices, and go fishing for where we believe the device drivers
        // themselves to be.  It does not need to be said that this is
        // operating by convention; there are (many) ways to envision this
        // breaking -- with apologies, dear reader, if that's what brings you
        // here!
        //
        use cargo_metadata::MetadataCommand;

        let metadata = MetadataCommand::new()
            .manifest_path("./Cargo.toml")
            .exec()
            .unwrap();

        let pkg = metadata
            .packages
            .iter()
            .find(|p| p.name == "drv-i2c-devices")
            .context("failed to find drv-i2c-devices")?;

        let dir = pkg
            .manifest_path
            .parent()
            .context("failed to get i2c device path")?;

        let mut drivers = std::collections::HashSet::new();

        println!("cargo:rerun-if-changed={}", dir.join("src").display());

        for entry in std::fs::read_dir(dir.join("src"))? {
            if let Some(f) = entry?.path().file_name() {
                if let Some(name) = f.to_str().unwrap().strip_suffix(".rs") {
                    drivers.insert(name.to_string());
                }
            }
        }

        drivers.remove("lib");

        write!(
            &mut self.output,
            r##"
    pub mod validation {{
        #[allow(unused_imports)]
        use drv_i2c_api::{{I2cDevice, Controller, PortIndex}};
        #[allow(unused_imports)]
        use drv_i2c_devices::Validate;
        use userlib::TaskId;

        #[allow(dead_code)]
        pub enum I2cValidation {{
            RawReadOk,
            Good,
            Bad,
        }}

        #[allow(unused_variables)]
        pub fn validate(
            task: TaskId,
            index: usize,
        ) -> Result<I2cValidation, drv_i2c_api::ResponseCode> {{
            match index {{"##
        )?;

        for (index, device) in self.devices.iter().enumerate() {
            if drivers.get(&device.device).is_some() {
                let driver = device.device.to_case(Case::UpperCamel);
                let out = self.generate_device(device, 24);

                write!(
                    &mut self.output,
                    r##"
                {} => {{
                    if drv_i2c_devices::{}::{}::validate(&{})? {{
                        Ok(I2cValidation::Good)
                    }} else {{
                        Ok(I2cValidation::Bad)
                    }}
                }}"##,
                    index, device.device, driver, out
                )?;
            } else {
                let out = self.generate_device(device, 20);
                write!(
                    &mut self.output,
                    r##"
                {} => {{{}.read::<u8>()?;
                    Ok(I2cValidation::RawReadOk)
                }}"##,
                    index, out
                )?;
            }
        }

        writeln!(
            &mut self.output,
            r##"
                _ => Err(drv_i2c_api::ResponseCode::BadArg)
            }}
        }}
    }}"##
        )?;

        Ok(())
    }

    pub fn generate_pmbus(&mut self) -> Result<()> {
        let mut byrail = HashMap::new();

        for d in &self.devices {
            if let Some(pmbus) = &d.pmbus {
                if let Some(rails) = &pmbus.rails {
                    for (index, rail) in rails.iter().enumerate() {
                        if rail.is_empty() {
                            continue;
                        }

                        if byrail.insert(rail, (d, index)).is_some() {
                            panic!("duplicate rail {}", rail);
                        }
                    }
                }
            }
        }

        if !byrail.is_empty() {
            write!(
                &mut self.output,
                r##"
    pub mod pmbus {{
        use drv_i2c_api::{{I2cDevice, Controller, PortIndex}};
        use userlib::TaskId;
"##
            )?;

            for (rail, (device, index)) in &byrail {
                write!(
                    &mut self.output,
                    r##"
        #[allow(dead_code)]
        pub fn {}(task: TaskId) -> (I2cDevice, u8) {{"##,
                    rail.to_lowercase(),
                )?;

                let out = self.generate_device(device, 16);
                writeln!(&mut self.output, "({}, {})\n        }}", out, index)?;
            }

            writeln!(&mut self.output, "    }}")?;
        }
        Ok(())
    }

    fn emit_sensor(
        &mut self,
        device: &str,
        label: &str,
        ids: &[usize],
    ) -> Result<()> {
        writeln!(
            &mut self.output,
            r##"
        #[allow(dead_code)]
        pub const NUM_{}_{}_SENSORS: usize = {};"##,
            device.to_uppercase(),
            label,
            ids.len(),
        )?;

        if ids.len() == 1 {
            writeln!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub const {}_{}_SENSOR: SensorId = SensorId({});"##,
                device.to_uppercase(),
                label,
                ids[0]
            )?;
        } else {
            writeln!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub const {}_{}_SENSORS: [SensorId; {}] = [ "##,
                device.to_uppercase(),
                label,
                ids.len(),
            )?;

            for id in ids {
                writeln!(&mut self.output, "            SensorId({}), ", id)?;
            }

            writeln!(&mut self.output, "        ];")?;
        }

        Ok(())
    }

    pub fn generate_sensors(&mut self) -> Result<()> {
        let mut bydevice = MultiMap::new();
        let mut byname = MultiMap::new();
        let mut bybus = MultiMap::new();
        let mut bybusname = MultiMap::new();
        let mut bykind = MultiMap::new();

        let mut sensors = vec![];

        let mut add_sensor = |kind, d: &I2cDevice, idx: usize| {
            let id = sensors.len();
            sensors.push(kind);

            let name: Option<String> = if let Some(pmbus) = &d.pmbus {
                if let Some(rails) = &pmbus.rails {
                    if idx < rails.len() {
                        Some(rails[idx].clone())
                    } else {
                        panic!("sensor count exceeds rails for {:?}", d);
                    }
                } else {
                    d.name.clone()
                }
            } else if let Some(names) = &d.sensors.as_ref().unwrap().names {
                if idx >= names.len() {
                    panic!(
                        "name array is too short ({}) for sensor index ({})",
                        names.len(),
                        idx
                    );
                } else {
                    Some(names[idx].clone())
                }
            } else {
                d.name.clone()
            };

            if let Some(bus) = &d.bus {
                bybus.insert((d.device.clone(), bus.clone(), kind), id);

                if let Some(ref name) = name {
                    bybusname.insert(
                        (d.device.clone(), bus.clone(), name.clone(), kind),
                        id,
                    );
                }
            }

            if let Some(name) = name {
                byname.insert((d.device.clone(), name, kind), id);
            }

            bydevice.insert((d.device.clone(), kind), id);
            bykind.insert(kind, id);
        };

        for d in &self.devices {
            if let Some(s) = &d.sensors {
                for i in 0..s.temperature {
                    add_sensor(Sensor::Temperature, d, i);
                }

                for i in 0..s.power {
                    add_sensor(Sensor::Power, d, i);
                }

                for i in 0..s.current {
                    add_sensor(Sensor::Current, d, i);
                }

                for i in 0..s.voltage {
                    add_sensor(Sensor::Voltage, d, i);
                }

                for i in 0..s.speed {
                    add_sensor(Sensor::Speed, d, i);
                }
            }
        }

        write!(
            &mut self.output,
            r##"
    pub mod sensors {{
        use task_sensor_api::SensorId;

        #[allow(dead_code)]
        pub const NUM_SENSORS: usize = {};
"##,
            sensors.len()
        )?;

        for ((device, kind), ids) in bydevice.iter_all() {
            self.emit_sensor(device, &format!("{}", kind), ids)?;
        }

        for ((device, name, kind), ids) in byname.iter_all() {
            let label = format!("{}_{}", name.to_uppercase(), kind);
            self.emit_sensor(device, &label, ids)?;
        }

        for ((device, bus, kind), ids) in bybus.iter_all() {
            let label = format!("{}_{}", bus.to_uppercase(), kind);
            self.emit_sensor(device, &label, ids)?;
        }

        for ((device, bus, name, kind), ids) in bybusname.iter_all() {
            let label = format!(
                "{}_{}_{}",
                bus.to_uppercase(),
                name.to_uppercase(),
                kind
            );
            self.emit_sensor(device, &label, ids)?;
        }

        writeln!(&mut self.output, "\n    }}")?;
        Ok(())
    }

    pub fn generate_ports(&mut self) -> Result<()> {
        writeln!(
            &mut self.output,
            r##"
    pub mod ports {{"##
        )?;

        for ((controller, port), index) in &self.ports {
            writeln!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub const fn i2c{controller}_{port}() -> drv_i2c_api::PortIndex {{
            drv_i2c_api::PortIndex({index})
        }}"##,
                controller = controller,
                port = port.to_case(Case::Snake),
                index = index,
            )?;
        }

        writeln!(&mut self.output, "    }}")?;
        Ok(())
    }
}

pub fn codegen(disposition: Disposition) -> Result<()> {
    use std::io::Write;

    let out_dir = env::var("OUT_DIR")?;
    let dest_path = Path::new(&out_dir).join("i2c_config.rs");
    let mut file = File::create(&dest_path)?;

    let mut g = ConfigGenerator::new(disposition);

    g.generate_header()?;

    match disposition {
        Disposition::Target => {
            let n = g.ncontrollers();

            if n != 1 {
                //
                // If we have the disposition of a target, we expect exactly one
                // controller to be configured as a target; if none have been
                // specified, the task should be deconfigured.
                //
                panic!("found {} I2C controller(s); expected exactly one", n);
            }

            g.generate_controllers()?;
            g.generate_pins()?;
            g.generate_ports()?;
        }

        Disposition::Initiator => {
            g.generate_controllers()?;
            g.generate_pins()?;
            g.generate_ports()?;
            g.generate_muxes()?;
        }

        Disposition::Devices => {
            g.generate_devices()?;
            g.generate_pmbus()?;
        }

        Disposition::Sensors => {
            g.generate_devices()?;
            g.generate_pmbus()?;
            g.generate_sensors()?;
        }

        Disposition::Validation => {
            g.generate_validation()?;
        }
    }

    g.generate_footer()?;

    file.write_all(g.output.as_bytes())?;

    Ok(())
}
