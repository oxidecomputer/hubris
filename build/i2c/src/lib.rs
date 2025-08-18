// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{bail, Context, Result};
use convert_case::{Case, Casing};
use indexmap::IndexMap;
use multimap::MultiMap;
use rangemap::RangeSet;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::fs::File;

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
// (i.e., [`power`]) into optional fields in [`I2cDevice`].  This makes it
// easier to accidentally create invalid entries (e.g., a device that has both
// a controller *and* a named bus), so the validation code should go to
// additional lengths to assure that these mistakes are caught in compilation.
//
#[derive(Clone, Debug, Deserialize, PartialOrd, Ord, Eq, PartialEq)]
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

    /// Disambiguation between sensor configurations
    flavor: Option<String>,

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

    /// reference designator suffix, if any
    refdes_suffix: Option<String>,

    /// power information, if any
    power: Option<I2cPower>,

    /// sensor information, if any
    sensors: Option<I2cSensors>,

    /// device is removable
    #[serde(default)]
    removable: bool,
}

impl I2cDevice {
    /// Checks whether the given sensor kind is associated with an `I2cPower`
    /// struct stored in this device, returning it if that's the case.
    ///
    /// In most cases, when the power member variable is present, sensors have a
    /// one-to-one association with power rails.  However, this isn't always
    /// true: in the power shelf, for example, there are two rails and three
    /// (uncorrelated) temperature sensors.
    ///
    /// This is indicated with the `sensors` array, which allows us to specify
    /// only certain kinds of sensors being tied to rails.
    ///
    /// If the `sensors` array is `None`, then we fall back to the default case
    /// of all sensors being one-to-one associated with rails.
    fn power_for_kind(&self, kind: Sensor) -> Option<&I2cPower> {
        self.power.as_ref().filter(|power| {
            power.sensors.as_ref().is_none_or(|s| s.contains(&kind))
        })
    }

    fn device_id(&self) -> Option<String> {
        let refdes = self.refdes.as_ref()?;
        Some(if let Some(ref suffix) = self.refdes_suffix {
            format!("{refdes}/{suffix}")
        } else {
            refdes.clone()
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct I2cPort {
    name: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
    scl: I2cPin,
    sda: I2cPin,
    af: u8,
    #[serde(default)]
    muxes: Vec<I2cMux>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cPin {
    gpio_port: Option<String>,
    pin: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cGpio {
    port: String,
    pin: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct I2cMux {
    driver: String,
    address: u8,
    #[serde(alias = "enable")]
    nreset: Option<I2cGpio>,
}

#[derive(Clone, Debug, Deserialize, PartialOrd, PartialEq, Eq, Ord)]
#[allow(dead_code)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct I2cPower {
    rails: Option<Vec<String>>,

    /// Optional phases, which must be the same length as `rails` if present
    phases: Option<Vec<Vec<u8>>>,

    #[serde(default = "I2cPower::default_pmbus")]
    pmbus: bool,

    /// Lists which sensor types have a one-to-one association with power rails
    ///
    /// When `None`, we assume that all sensor types are mapped one-to-one with
    /// rails.  Otherwise, *only* the listed sensor types are associated with
    /// rails (which is the case in systems with independent temperature sensors
    /// and power rails).
    sensors: Option<Vec<Sensor>>,
}

impl I2cPower {
    fn default_pmbus() -> bool {
        true
    }
}

#[derive(Clone, Debug, Deserialize, PartialOrd, PartialEq, Eq, Ord)]
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
    input_current: usize,

    #[serde(default)]
    input_voltage: usize,

    #[serde(default)]
    speed: usize,

    names: Option<Vec<String>>,
}

impl I2cSensors {
    /// Checks whether two sensor sets are compatible
    ///
    /// "Compatible" means that they have the same number of sensors in each
    /// category, meaning they can be represented by the same `struct`.
    fn is_compatible_with(&self, other: &Self) -> bool {
        // Manually unpack the struct, so that any new sensor types have to be
        // updated here!
        let &Self {
            temperature,
            power,
            current,
            voltage,
            input_current,
            input_voltage,
            speed,
            names: _,
        } = self;
        temperature == other.temperature
            && power == other.power
            && current == other.current
            && voltage == other.voltage
            && input_current == other.input_current
            && input_voltage == other.input_voltage
            && speed == other.speed
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
struct DeviceKey {
    device: String,
    kind: Sensor,
}

#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
struct DeviceNameKey {
    device: String,
    name: String,
    kind: Sensor,
}

#[derive(Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
struct DeviceRefdesKey {
    device: String,
    refdes: String,
    suffix: Option<String>,
    kind: Sensor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSensor {
    pub name: Option<String>,
    pub kind: Sensor,
    pub id: usize,
}

#[derive(Debug)]
struct I2cSensorsDescription {
    // In all multimaps below, the value is the sensor ID. The same sensor ID
    // can show up in multiple (including all!) of these maps.
    //
    // All sensors are guaranteed to be present in `by_device`, but
    // may not be present in the other maps (devices may or may not have a
    // name/bus in app.toml).
    by_device: MultiMap<DeviceKey, usize>,
    by_name: MultiMap<DeviceNameKey, usize>,
    by_refdes: MultiMap<DeviceRefdesKey, usize>,

    // list of all devices and a list of their sensors, with an optional sensor
    // name (if present)
    device_sensors: Vec<Vec<DeviceSensor>>,

    total_sensors: usize,
}

impl I2cSensorsDescription {
    fn new(devices: &[I2cDevice]) -> Self {
        let mut desc = Self {
            by_device: MultiMap::with_capacity(devices.len()),
            by_name: MultiMap::new(),
            by_refdes: MultiMap::new(),
            device_sensors: vec![Vec::new(); devices.len()],
            total_sensors: 0,
        };

        for (d_index, d) in devices.iter().enumerate() {
            if let Some(s) = &d.sensors {
                for i in 0..s.temperature {
                    desc.add_sensor(Sensor::Temperature, d, i, d_index);
                }

                for i in 0..s.power {
                    desc.add_sensor(Sensor::Power, d, i, d_index);
                }

                for i in 0..s.current {
                    desc.add_sensor(Sensor::Current, d, i, d_index);
                }

                for i in 0..s.voltage {
                    desc.add_sensor(Sensor::Voltage, d, i, d_index);
                }

                for i in 0..s.input_current {
                    desc.add_sensor(Sensor::InputCurrent, d, i, d_index);
                }

                for i in 0..s.input_voltage {
                    desc.add_sensor(Sensor::InputVoltage, d, i, d_index);
                }

                for i in 0..s.speed {
                    desc.add_sensor(Sensor::Speed, d, i, d_index);
                }
            }
        }

        desc
    }

    // `idx` is the index of the type of sensor within `d` (the idx-th
    // temperature sensor or the idx-th power sensor, etc.; see the loop in
    // `new()` above).
    //
    // `dev_index` is the index of `d` within the total list of devices.
    //
    // This method should only be called by `new()`. It fills out `self`'s
    // fields as it is being constructed.
    fn add_sensor(
        &mut self,
        kind: Sensor,
        d: &I2cDevice,
        idx: usize,
        dev_index: usize,
    ) {
        let id = self.total_sensors;
        self.total_sensors += 1;

        let name: Option<String> = if let Some(power) = d.power_for_kind(kind) {
            if let Some(rails) = &power.rails {
                if idx < rails.len() {
                    Some(rails[idx].clone())
                } else {
                    panic!("sensor count exceeds rails for {d:?}",);
                }
            } else {
                d.name.clone()
            }
        } else if let Some(names) = &d.sensors.as_ref().unwrap().names {
            if idx >= names.len() {
                panic!(
                    "name array is too short ({}) for sensor index ({idx})",
                    names.len(),
                );
            } else {
                Some(names[idx].clone())
            }
        } else {
            d.name.clone()
        };

        if let Some(name) = name.clone() {
            self.by_name.insert(
                DeviceNameKey {
                    device: d.device.clone(),
                    name,
                    kind,
                },
                id,
            );
        }

        if let Some(refdes) = d.refdes.clone() {
            self.by_refdes.insert(
                DeviceRefdesKey {
                    device: d.device.clone(),
                    refdes,
                    suffix: d.refdes_suffix.clone(),
                    kind,
                },
                id,
            );
        }

        self.by_device.insert(
            DeviceKey {
                device: d.device.clone(),
                kind,
            },
            id,
        );
        self.device_sensors[dev_index].push(DeviceSensor { name, kind, id });
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
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

#[derive(
    Copy, Clone, Deserialize, Debug, PartialEq, Eq, Hash, Ord, PartialOrd,
)]
#[serde(rename_all = "kebab-case")]
pub enum Sensor {
    Temperature,
    Power,
    Current,
    Voltage,
    InputCurrent,
    InputVoltage,
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
                Sensor::InputCurrent => "INPUT_CURRENT",
                Sensor::InputVoltage => "INPUT_VOLTAGE",
                Sensor::Speed => "SPEED",
            }
        )
    }
}

#[derive(PartialEq)]
enum PowerDevices {
    /// PMBus power devices
    PMBus,

    /// Non-PMBus power devices
    NonPMBus,
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

    /// if `true`, include refdes string in output
    include_refdes: bool,
}

impl ConfigGenerator {
    fn new(settings: CodegenSettings) -> Self {
        let CodegenSettings {
            disposition,
            include_refdes,
        } = settings;
        let i2c = match build_util::config::<Config>() {
            Ok(config) => config.i2c,
            Err(err) => {
                panic!("malformed config.i2c: {err:?}");
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
                        panic!("i2c bus {name} appears twice");
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
                    (_, Some(bus)) if !buses.contains_key(bus) => {
                        panic!(
                            "device {} at address {:#x} specifies \
                            unknown bus \"{bus}\"",
                            d.device, d.address,
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
            include_refdes,
        }
    }

    pub fn ncontrollers(&self) -> usize {
        self.controllers.len()
    }

    pub fn generate_header(&mut self) -> Result<()> {
        writeln!(&mut self.output, "pub(crate) mod i2c_config {{")?;
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
        use drv_i2c_api::Controller;"##
            )?;

            if build_util::has_feature("h743") {
                writeln!(&mut s, "use stm32h7::stm32h743 as device;")?;
            }
            if build_util::has_feature("h753") {
                writeln!(&mut s, "use stm32h7::stm32h753 as device;")?;
            }
            if build_util::has_feature("g031") {
                writeln!(&mut s, "use stm32g0::stm32g031 as device;")?;
            }
            if build_util::has_feature("g030") {
                writeln!(&mut s, "use stm32g0::stm32g030 as device;")?;
            }
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
                notification: crate::notifications::I2C{controller}_IRQ_MASK,
                registers: unsafe {{ &*device::I2C{controller}::ptr() }},
            }},"##,
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
            len += c.ports.len();
        }

        writeln!(
            &mut s,
            r##"
    #[allow(unused_imports)]
    use drv_stm32xx_i2c::{{I2cPins, I2cGpio}};

    pub fn pins() -> [I2cPins; {len}] {{"##,
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
                writeln!(
                    &mut s,
                    r##"
            I2cPins {{
                controller: Controller::I2C{controller},
                port: PortIndex({index}),
                scl: gpio_api::Port::{scl}.pin({scl_pin}),
                sda: gpio_api::Port::{sda}.pin({sda_pin}),
                function: Alternate::AF{af},
            }},"##,
                    controller = c.controller,
                    scl = match port.scl.gpio_port {
                        Some(ref port) => port,
                        None => p,
                    },
                    scl_pin = port.scl.pin,
                    sda = match port.sda.gpio_port {
                        Some(ref port) => port,
                        None => p,
                    },
                    sda_pin = port.sda.pin,
                    af = port.af
                )?;
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

    pub fn muxes() -> [I2cMux<'static>; {len}] {{"##,
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
            for (index, port) in c.ports.values().enumerate() {
                for (mindex, mux) in port.muxes.iter().enumerate() {
                    let nreset = mux
                        .nreset
                        .as_ref()
                        .map(|enable| {
                            format!(
                                r##"Some(I2cGpio {{
                    gpio_pins: gpio_api::Port::{gpio_port}.pin({gpio_pin}),
                }})"##,
                                gpio_port = enable.port,
                                gpio_pin = enable.pin,
                            )
                        })
                        .unwrap_or_else(|| "None".to_string());

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
                nreset: {nreset},
                address: {address:#x},
            }},"##,
                        controller = c.controller,
                        i2c_port = index,
                        mindex = mindex + 1,
                        driver = mux.driver,
                        driver_struct = driver_struct,
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

    fn lookup_controller_port(&self, d: &I2cDevice) -> (u8, usize) {
        let controller = match &d.bus {
            Some(bus) => self.buses.get(bus).unwrap().0,
            None => d.controller.unwrap(),
        };

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

        (controller, *port)
    }

    fn generate_device(&self, d: &I2cDevice, indent: usize) -> String {
        let (controller, port) = self.lookup_controller_port(d);

        let segment = match (d.mux, d.segment) {
            (Some(mux), Some(segment)) => {
                let mux_count = self
                    .controllers
                    .iter()
                    .find(|c| c.controller == controller)
                    .unwrap()
                    .ports
                    .values()
                    .nth(port)
                    .unwrap()
                    .muxes
                    .len();
                if mux == 0 {
                    panic!(
                        "invalid mux value of 0 for {d:?} \
                        (note that muxes are 1-indexed)"
                    );
                } else if usize::from(mux) > mux_count {
                    panic!(
                        "invalid mux {mux} for {d:?} (must be <= {mux_count})"
                    );
                }
                format!(
                    "Some((drv_i2c_api::Mux::M{mux}, drv_i2c_api::Segment::S{segment}))",
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

        let indent = format!("{:indent$}", "", indent = indent);

        let refdes_part = if self.include_refdes {
            d.device_id()
                .map(|id| format!(".with_refdes({id:?})"))
                .unwrap_or(String::new())
        } else {
            String::new()
        };

        format!(
            r##"
{indent}// {description}
{indent}I2cDevice::new(task,
{indent}    Controller::I2C{controller},
{indent}    PortIndex({port}),
{indent}    {segment},
{indent}    {address:#x}
{indent}){refdes_part}"##,
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
        let mut by_device = MultiMap::new();
        let mut by_name = HashMap::new();
        let mut by_refdes = HashMap::new();
        let mut by_bus = MultiMap::new();

        let mut by_port = MultiMap::new();
        let mut by_controller = MultiMap::new();

        for (index, d) in self.devices.iter().enumerate() {
            by_device.insert(&d.device, d);

            let (controller, port) = self.lookup_controller_port(d);

            by_port.insert(port, index);
            by_controller.insert(controller, index);

            if let Some(bus) = &d.bus {
                by_bus.insert((&d.device, bus), d);
            }

            if let Some(name) = &d.name {
                if by_name.insert((&d.device, name), d).is_some() {
                    panic!("duplicate name {} for device {}", name, d.device)
                }
            }
            if let Some(refdes) = &d.refdes {
                if by_refdes
                    .insert((&d.device, refdes, d.refdes_suffix.as_ref()), d)
                    .is_some()
                {
                    panic!(
                        "duplicate refdes {refdes}{}{} for device {}",
                        if d.refdes_suffix.is_some() { "/" } else { "" },
                        d.refdes_suffix.as_deref().unwrap_or(""),
                        d.device
                    )
                } else if by_name.contains_key(&(&d.device, refdes)) {
                    panic!(
                        "refdes {} for device {} is also a device name",
                        refdes, d.device
                    )
                }
            }
        }

        write!(
            &mut self.output,
            r##"
    pub mod devices {{
        #[allow(unused_imports)]
        use drv_i2c_api::{{I2cDevice, Controller, PortIndex}};
        #[allow(unused_imports)]
        use userlib::TaskId;
"##
        )?;

        write!(
            &mut self.output,
            r##"
        #[allow(dead_code)]
        #[allow(clippy::match_single_binding)]
        pub fn lookup_controller(index: usize) -> Option<Controller> {{
            match index {{"##
        )?;

        let mut all: Vec<_> = by_controller.iter_all().collect();
        all.sort();

        match_arms(&mut self.output, all, |c| {
            format!("Some(Controller::I2C{c})")
        })?;

        write!(
            &mut self.output,
            r##"
                _ => None
            }}
        }}
"##
        )?;

        write!(
            &mut self.output,
            r##"
        #[allow(dead_code)]
        #[allow(clippy::match_single_binding)]
        pub fn lookup_port(index: usize) -> Option<PortIndex> {{
            match index {{"##
        )?;

        let mut all: Vec<_> = by_port.iter_all().collect();
        all.sort();

        match_arms(&mut self.output, all, |p| format!("Some(PortIndex({p}))"))?;

        write!(
            &mut self.output,
            r##"
                _ => None
            }}
        }}
"##
        )?;

        let mut all: Vec<_> = by_device.iter_all().collect();
        all.sort();

        for (device, devices) in all {
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
                write!(&mut self.output, "{out},")?;
            }

            writeln!(
                &mut self.output,
                r##"
            ]
        }}"##
            )?;
        }

        let mut all: Vec<_> = by_bus.iter_all().collect();
        all.sort();

        for ((device, bus), devices) in all {
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
                write!(&mut self.output, "{out},")?;
            }
            writeln!(
                &mut self.output,
                r##"
            ]
        }}"##
            )?;
        }

        let mut all: Vec<_> = by_name.iter().collect();
        all.sort();
        for ((device, name), d) in &all {
            write!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub fn {}_{}(task: TaskId) -> I2cDevice {{"##,
                device,
                name.to_lowercase()
            )?;

            let out = self.generate_device(d, 16);
            write!(&mut self.output, "{out}")?;

            writeln!(
                &mut self.output,
                r##"
        }}"##
            )?;
        }

        let mut all: Vec<_> = by_refdes.iter().collect();
        all.sort();

        for ((device, name, suffix), d) in &all {
            let name = name.to_lowercase();
            let suffix_sep = if suffix.is_some() { "_" } else { "" };
            let suffix =
                suffix.map(|s| s.to_lowercase()).unwrap_or_else(String::new);
            write!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub fn {device}_{name}{suffix_sep}{suffix}(task: TaskId) -> I2cDevice {{"##,
            )?;

            let out = self.generate_device(d, 16);
            write!(&mut self.output, "{out}")?;

            writeln!(
                &mut self.output,
                r##"
        }}"##
            )?;
        }

        writeln!(&mut self.output, "    }}")?;

        self.generate_power(PowerDevices::PMBus)?;
        self.generate_power(PowerDevices::NonPMBus)?;

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

        println!("cargo::rerun-if-changed={}", dir.join("src").display());

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
        #[allow(clippy::match_single_binding)]
        pub fn validate(
            task: TaskId,
            index: usize,
        ) -> Result<I2cValidation, drv_i2c_api::ResponseCode> {{
            match index {{"##
        )?;

        // The ordering / index values of this `match` must match the ordering
        // returned by `device_descriptions()` below: if we change the ordering
        // here, it must be updated there as well.
        for (index, device) in self.devices.iter().enumerate() {
            if drivers.contains(&device.device) {
                let driver = device.device.to_case(Case::UpperCamel);
                let out = self.generate_device(device, 24);

                write!(
                    &mut self.output,
                    r##"
                {index} => {{
                    if drv_i2c_devices::{device}::{driver}::validate(&{out})? {{
                        Ok(I2cValidation::Good)
                    }} else {{
                        Ok(I2cValidation::Bad)
                    }}
                }}"##,
                    device = device.device,
                )?;
            } else {
                let out = self.generate_device(device, 20);
                write!(
                    &mut self.output,
                    r##"
                {index} => {{{out}.read::<u8>()?;
                    Ok(I2cValidation::RawReadOk)
                }}"##,
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

    fn generate_power(&mut self, which: PowerDevices) -> Result<()> {
        let mut byrail = HashMap::new();

        for d in &self.devices {
            if let Some(power) = &d.power {
                if power.pmbus && which != PowerDevices::PMBus {
                    continue;
                }

                //
                // If we have phases, we must have phases for each rail -- and
                // we check that no single phase is present in more than one
                // rail.
                //
                match (&power.rails, &power.phases) {
                    (Some(_), None) | (None, None) => {}
                    (Some(r), Some(p)) if r.len() == p.len() => {
                        let mut all = HashSet::new();

                        if let Some(p) =
                            p.iter().flatten().find(|&p| !all.insert(p))
                        {
                            bail!("phase {p} appears multiple times in {d:?}");
                        }
                    }
                    _ => {
                        bail!("rail/phase length mismatch on {d:?}");
                    }
                }

                if let Some(rails) = &power.rails {
                    for (index, rail) in rails.iter().enumerate() {
                        if rail.is_empty() {
                            continue;
                        }

                        if byrail.insert(rail, (d, index)).is_some() {
                            bail!("duplicate rail {}", rail);
                        }
                    }
                }
            }
        }

        if !byrail.is_empty() {
            write!(
                &mut self.output,
                r##"
    pub mod {} {{
        use drv_i2c_api::{{I2cDevice, Controller, PortIndex}};
        use userlib::TaskId;
"##,
                match which {
                    PowerDevices::PMBus => "pmbus",
                    PowerDevices::NonPMBus => "power",
                }
            )?;

            let mut all: Vec<_> = byrail.iter().collect();
            all.sort();

            for (rail, (device, index)) in &all {
                write!(
                    &mut self.output,
                    r##"
        #[allow(dead_code)]
        pub fn {}(task: TaskId) -> (I2cDevice, u8) {{"##,
                    rail.to_lowercase(),
                )?;

                let out = self.generate_device(device, 16);
                writeln!(&mut self.output, "({out}, {index})\n        }}")?;

                if which == PowerDevices::PMBus {
                    let phases = if let Some(power) = &device.power {
                        if let Some(phases) = &power.phases {
                            let p = phases[*index]
                                .iter()
                                .map(|p| p.to_string())
                                .collect::<Vec<_>>()
                                .join(", ");

                            format!("Some(&[{p}])")
                        } else {
                            "None".to_string()
                        }
                    } else {
                        "None".to_string()
                    };

                    writeln!(
                        &mut self.output,
                        r##"
        #[allow(dead_code)]
        pub const {}_{rail}_PHASES: Option<&'static [u8]> = {phases};"##,
                        device.device.to_uppercase()
                    )?;
                }
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
        let device = device.to_uppercase();
        let n_sensors = ids.len();
        writeln!(
            &mut self.output,
            r##"
        #[allow(dead_code)]
        pub const NUM_{device}_{label}_SENSORS: usize = {n_sensors};"##,
        )?;

        if ids.len() == 1 {
            writeln!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub const {device}_{label}_SENSOR: SensorId = SensorId::new({});"##,
                ids[0]
            )?;
        } else {
            writeln!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub const {device}_{label}_SENSORS: [SensorId; {n_sensors}] = [ "##,
            )?;

            for id in ids {
                writeln!(&mut self.output, "            SensorId::new({id}),",)?;
            }

            writeln!(&mut self.output, "        ];")?;
        }

        Ok(())
    }

    fn declare_sensor_struct(
        &mut self,
        d: &I2cDevice,
        struct_name: &str,
    ) -> Result<()> {
        // Manually unpack the field so that changes to the sensor types
        // will require changes here as well.
        if let Some(I2cSensors {
            temperature,
            power,
            current,
            voltage,
            input_current,
            input_voltage,
            speed,
            names: _,
        }) = &d.sensors
        {
            writeln!(
                &mut self.output,
                "\n        #[allow(non_camel_case_types, dead_code)]
        pub struct Sensors_{struct_name} {{",
            )?;
            let mut f = |name, count| match count {
                0 => Ok(()),
                1 => writeln!(
                    &mut self.output,
                    "            pub {name}: SensorId,"
                ),
                _ => writeln!(
                    &mut self.output,
                    "            pub {name}: [SensorId; {count}],"
                ),
            };
            f("temperature", *temperature)?;
            f("power", *power)?;
            f("current", *current)?;
            f("voltage", *voltage)?;
            f("input_current", *input_current)?;
            f("input_voltage", *input_voltage)?;
            f("speed", *speed)?;
            writeln!(&mut self.output, "        }}")?;
        } else {
            writeln!(
                &mut self.output,
                "\n        #[allow(dead_code, non_camel_case_types)]
        type Sensors_{struct_name} = ();",
            )?;
        }
        Ok(())
    }

    fn emit_sensor_struct(
        &mut self,
        d: &I2cDevice,
        label: String,
        name: &str,
        sensors: &[DeviceSensor],
    ) -> Result<()> {
        write!(
            &mut self.output,
            "        #[allow(dead_code)]
        pub const {}_{label}_SENSORS: Sensors_{name} = ",
            d.device.to_uppercase(),
        )?;

        let mut sensors_by_kind: BTreeMap<Sensor, Vec<usize>> = BTreeMap::new();
        for s in sensors {
            sensors_by_kind.entry(s.kind).or_default().push(s.id);
        }
        if sensors_by_kind.is_empty() {
            writeln!(&mut self.output, "();")?;
            return Ok(());
        }

        writeln!(&mut self.output, "Sensors_{name} {{")?;

        for (kind, values) in sensors_by_kind {
            let field = match kind {
                Sensor::Temperature => "temperature",
                Sensor::Power => "power",
                Sensor::Current => "current",
                Sensor::Voltage => "voltage",
                Sensor::InputCurrent => "input_current",
                Sensor::InputVoltage => "input_voltage",
                Sensor::Speed => "speed",
            };
            if values.len() == 1 {
                writeln!(
                    &mut self.output,
                    "            {field}: SensorId::new({}),",
                    values[0]
                )?;
            } else {
                write!(&mut self.output, "            {field}: [")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        write!(&mut self.output, ", ")?;
                    }
                    write!(&mut self.output, "SensorId::new({v})")?;
                }
                writeln!(&mut self.output, "],")?;
            }
        }

        writeln!(&mut self.output, "        }};")?;
        Ok(())
    }

    fn sensors_description(&self) -> I2cSensorsDescription {
        I2cSensorsDescription::new(&self.devices)
    }

    pub fn generate_sensors(&mut self) -> Result<()> {
        let s = self.sensors_description();

        write!(
            &mut self.output,
            r##"
    pub mod sensors {{
        #[allow(unused_imports)]
        use super::super::SensorId;

        #[allow(dead_code)]
        pub const NUM_SENSORS: usize = {};
"##,
            s.total_sensors
        )?;

        let mut emitted_structs: HashMap<String, Option<I2cSensors>> =
            HashMap::new();
        for (i, d) in self.devices.clone().iter().enumerate() {
            let mut struct_name = d.device.clone();
            if let Some(suffix) = &d.flavor {
                struct_name = format!("{struct_name}_{suffix}");
            }
            if let Some(prev) = emitted_structs.get(&struct_name) {
                match (prev, &d.sensors) {
                    (Some(a), Some(b)) => {
                        if !a.is_compatible_with(b) {
                            panic!(
                                "I2C device {struct_name} is declared with \
                                 inconsistent numbers of sensors.  Add a \
                                 `flavor = \"...\"` key to disambiguate."
                            );
                        }
                    }
                    (Some(..), None) | (None, Some(..)) => {
                        panic!(
                            "I2C device {struct_name} is declared both \
                             with and without sensors.  Use a \
                             `flavor = \"...\"` key to disambiguate."
                        );
                    }
                    (None, None) => (),
                }
            } else {
                emitted_structs.insert(struct_name.clone(), d.sensors.clone());
                self.declare_sensor_struct(d, &struct_name)?;
            }
            let s = s.device_sensors[i].as_slice();
            if let Some(name) = &d.name {
                self.emit_sensor_struct(
                    d,
                    name.to_uppercase(),
                    &struct_name,
                    s,
                )?;
            }
            if let Some(refdes) = &d.refdes {
                let refdes = refdes.to_uppercase();
                let label = if let Some(ref suffix) = d.refdes_suffix {
                    format!("{refdes}_{}", suffix.to_uppercase())
                } else {
                    refdes
                };
                self.emit_sensor_struct(d, label, &struct_name, s)?;
            }
        }

        let mut by_device_sorted: Vec<_> = s.by_device.iter_all().collect();
        by_device_sorted.sort();

        for (k, ids) in &by_device_sorted {
            self.emit_sensor(&k.device, &format!("{}", k.kind), ids)?;
        }

        let mut by_name_sorted: Vec<_> = s.by_name.iter_all().collect();
        by_name_sorted.sort();

        for (k, ids) in &by_name_sorted {
            let label = format!("{}_{}", k.name.to_uppercase(), k.kind);
            self.emit_sensor(&k.device, &label, ids)?;
        }

        let mut by_refdes_sorted: Vec<_> = s.by_refdes.iter_all().collect();
        by_refdes_sorted.sort();

        for (k, ids) in &by_refdes_sorted {
            let refdes = k.refdes.to_uppercase();
            let suffix_sep = if k.suffix.is_some() { "_" } else { "" };
            let suffix = k
                .suffix
                .as_ref()
                .map(|s| s.to_uppercase())
                .unwrap_or_else(String::new);
            let label = format!("{refdes}{suffix_sep}{suffix}_{}", k.kind);
            self.emit_sensor(&k.device, &label, ids)?;
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

#[derive(Copy, Clone)]
pub struct CodegenSettings {
    pub disposition: Disposition,
    pub include_refdes: bool,
}

impl From<Disposition> for CodegenSettings {
    fn from(disposition: Disposition) -> Self {
        CodegenSettings {
            disposition,
            include_refdes: false,
        }
    }
}

pub fn codegen(settings: impl Into<CodegenSettings>) -> Result<()> {
    let settings = settings.into();
    use std::io::Write;

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("i2c_config.rs");
    let mut file = File::create(dest_path)?;

    let mut g = ConfigGenerator::new(settings);

    g.generate_header()?;

    match settings.disposition {
        Disposition::Target => {
            let n = g.ncontrollers();

            if n != 1 {
                //
                // If we have the disposition of a target, we expect exactly one
                // controller to be configured as a target; if none have been
                // specified, the task should be deconfigured.
                //
                anyhow::bail!(
                    "found {n} I2C controller(s); expected exactly one"
                );
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
            g.generate_ports()?;
        }

        Disposition::Sensors => {
            g.generate_devices()?;
            g.generate_sensors()?;
        }

        Disposition::Validation => {
            g.generate_devices()?;
            g.generate_validation()?;
        }
    }

    g.generate_footer()?;

    file.write_all(g.output.as_bytes())?;

    Ok(())
}

pub struct I2cDeviceDescription {
    pub device: String,
    pub description: String,
    pub sensors: Vec<DeviceSensor>,
    pub device_id: Option<String>,
    pub name: Option<String>,
}

///
/// Returns a list of I2C device descriptions.
///
/// The order of device descriptions matches the indexing used in the generated
/// `validate()` command.
///
pub fn device_descriptions() -> impl Iterator<Item = I2cDeviceDescription> {
    let g = ConfigGenerator::new(Disposition::Validation.into());
    let sensors = g.sensors_description();

    assert_eq!(sensors.device_sensors.len(), g.devices.len());

    // Matches the ordering of the `match` produced by `generate_validation()`
    // above; if we change the order here, it must change there as well.
    g.devices.into_iter().zip(sensors.device_sensors).map(
        |(device, sensors)| {
            let device_id = device.device_id();
            I2cDeviceDescription {
                device: device.device,
                description: device.description,
                sensors,
                device_id,
                name: device.name,
            }
        },
    )
}

fn match_arms<'a, C>(
    mut out: impl Write,
    source: impl IntoIterator<Item = (&'a C, &'a Vec<usize>)>,
    fmt: impl Fn(&C) -> String,
) -> Result<()>
where
    C: 'a,
{
    for (controller, indices) in source {
        let indices = indices
            .iter()
            .map(|&i| i..i + 1)
            .collect::<RangeSet<usize>>();
        let s = indices
            .iter()
            .map(|range| format!("{}..={}", range.start, range.end - 1))
            .collect::<Vec<_>>()
            .join("\n                | ");

        let result = fmt(controller);

        write!(
            &mut out,
            r##"
                {s} => {result},"##,
        )?;
    }
    Ok(())
}
