// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{bail, Result};
use convert_case::{Case, Casing};
use indexmap::IndexMap;
use multimap::MultiMap;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fmt::Write;
use std::fs::File;
use std::path::Path;

//
// Our definition of the `Config` type.  We share this type with all other
// build-specific types; we must not set `deny_unknown_fields` here.
//
#[derive(Clone, Debug, Deserialize)]
struct Config {
    i2c: I2cConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cConfig {
    controllers: Vec<I2cController>,
    devices: Option<Vec<I2cDevice>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cController {
    controller: u8,
    ports: IndexMap<String, I2cPort>,
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
#[serde(deny_unknown_fields)]
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

    /// device is removable
    #[serde(default)]
    removable: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
struct I2cMux {
    driver: String,
    address: u8,
    enable: Option<I2cPinSet>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct I2cPmbus {
    rails: Option<Vec<String>>,
}

#[derive(Copy, Clone, PartialEq)]
pub enum Artifact {
    /// part of a complete distribution of an application
    Dist,

    /// standalone build of a single task
    Standalone,
}

#[derive(Copy, Clone, PartialEq)]
pub enum Disposition {
    /// controller is an initiator
    Initiator,

    /// controller is a target
    Target,

    /// only devices are used (i.e., controller is not used)
    Devices,
}

struct ConfigGenerator {
    /// output that we're building
    output: String,

    /// disposition of this configuration: target v. initiator v. devices
    disposition: Disposition,

    /// artifact that we're creating: standalone v. dist
    artifact: Artifact,

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
    fn new(disposition: Disposition, artifact: Artifact) -> Self {
        let i2c = match artifact {
            Artifact::Standalone => I2cConfig {
                controllers: vec![],
                devices: None,
            },
            Artifact::Dist => match build_util::config::<Config>() {
                Ok(config) => config.i2c,
                Err(err) => {
                    panic!("malformed config.i2c: {:?}", err);
                }
            },
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
                    match buses.insert(name.clone(), (c.controller, index)) {
                        Some(_) => {
                            panic!("i2c bus {} appears twice", name);
                        }
                        None => {}
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
                            "device {} at address 0x{:x} must have \
                            a bus or controller",
                            d.device, d.address
                        );
                    }
                    (Some(_), Some(_)) => {
                        panic!(
                            "device {} at address 0x{:x} has both \
                            a bus and a controller",
                            d.device, d.address
                        );
                    }
                    (_, Some(bus)) if buses.get(bus).is_none() => {
                        panic!(
                            "device {} at address 0x{:x} specifies \
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
            disposition: disposition,
            artifact: artifact,
            controllers: controllers,
            buses: buses,
            ports: ports,
            singletons: singletons,
            devices: i2c.devices.unwrap_or(Vec::new()),
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

        assert!(self.disposition != Disposition::Devices);

        writeln!(
            &mut s,
            r##"
    use drv_stm32h7_i2c::I2cController;

    pub fn controllers() -> [I2cController<'static>; {}] {{"##,
            self.controllers.len()
        )?;

        if self.controllers.len() > 0 {
            writeln!(
                &mut s,
                r##"
        use drv_stm32h7_rcc_api::Peripheral;
        use drv_i2c_api::Controller;

        #[cfg(feature = "h743")]
        use stm32h7::stm32h743 as device;

        #[cfg(feature = "h753")]
        use stm32h7::stm32h753 as device;

        #[cfg(feature = "h7b3")]
        use stm32h7::stm32h7b3 as device;"##
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
                notification: (1 << ({controller} - 1)),
                registers: unsafe {{ &*device::I2C{controller}::ptr() }},
            }},"##,
                controller = c.controller
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

        assert!(self.disposition != Disposition::Devices);

        for c in &self.controllers {
            for (_, port) in &c.ports {
                len += port.pins.len();
            }
        }

        writeln!(
            &mut s,
            r##"
    use drv_stm32h7_i2c::I2cPin;

    pub fn pins() -> [I2cPin; {}] {{"##,
            len
        )?;

        if len > 0 {
            writeln!(
                &mut s,
                r##"
        use drv_i2c_api::{{Controller, PortIndex}};
        use drv_stm32h7_gpio_api::{{self as gpio_api, Alternate}};"##
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
        let mut len = 0;

        for c in &self.controllers {
            for (_, port) in &c.ports {
                len += port.muxes.len();
            }
        }

        write!(
            &mut s,
            r##"
    use drv_stm32h7_i2c::I2cMux;

    pub fn muxes() -> [I2cMux<'static>; {}] {{"##,
            len
        )?;

        if len > 0 {
            writeln!(
                &mut s,
                r##"
        use drv_i2c_api::{{Controller, PortIndex, Mux}};

        #[allow(unused_imports)]
        use drv_stm32h7_gpio_api::{{self as gpio_api, Alternate}};"##
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
                        (&mux.driver[..1].to_string()).to_uppercase(),
                        &mux.driver[1..]
                    );

                    write!(
                        &mut s,
                        r##"
            I2cMux {{
                controller: Controller::I2C{controller},
                port: PortIndex({i2c_port}),
                id: Mux::M{mindex},
                driver: &drv_stm32h7_i2c::{driver}::{driver_struct},
                enable: {enable},
                address: 0x{address:x},
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

    fn generate_device(&self, d: &I2cDevice) -> String {
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

        format!(
            r##"
            // {description}
            I2cDevice::new(task,
                Controller::I2C{controller},
                PortIndex({port}),
                {segment},
                0x{address:x}
            )"##,
            description = d.description,
            controller = controller,
            port = port,
            segment = "None",
            address = d.address,
        )
    }

    pub fn generate_devices(&mut self) -> Result<()> {
        if self.artifact == Artifact::Standalone {
            //
            // For the standalone build, we generate a single, mock
            // device.
            //
            writeln!(
                &mut self.output,
                r##"
    pub mod devices {{
        use drv_i2c_api::I2cDevice;
        use userlib::TaskId;

        pub fn mock(task: TaskId) -> I2cDevice {{
            I2cDevice::mock(task)
        }}
    }}"##
            )?;
            return Ok(());
        }

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

                if let Some(name) = &d.name {
                    if byname.insert((&d.device, bus, name), d).is_some() {
                        panic!(
                            "duplicate name {} for device {} on bus {}",
                            name, d.device, bus
                        )
                    }
                }
            } else {
                if let Some(name) = &d.name {
                    panic!("named device {} is on unnamed bus", name);
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
                let out = self.generate_device(d);
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
                let out = self.generate_device(d);
                write!(&mut self.output, "{},", out)?;
            }
            writeln!(
                &mut self.output,
                r##"
            ]
        }}"##
            )?;
        }

        for ((device, bus, name), d) in &byname {
            write!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub fn {}_{}_{}(task: TaskId) -> I2cDevice {{"##,
                device, bus, name
            )?;

            let out = self.generate_device(d);
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

    pub fn generate_pmbus(&mut self) -> Result<()> {
        if self.artifact == Artifact::Standalone {
            return Ok(());
        }

        let mut byrail = HashMap::new();

        for d in &self.devices {
            if let Some(pmbus) = &d.pmbus {
                if let Some(rails) = &pmbus.rails {
                    for (index, rail) in rails.iter().enumerate() {
                        if rail.len() == 0 {
                            continue;
                        }

                        if byrail.insert(rail, (d, index)).is_some() {
                            panic!("duplicate rail {}", rail);
                        }
                    }
                }
            }
        }

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

            let out = self.generate_device(device);
            writeln!(&mut self.output, "({}, {})\n        }}", out, index)?;
        }

        writeln!(&mut self.output, "    }}")?;
        Ok(())
    }

    pub fn generate_ports(&mut self) -> Result<()> {
        writeln!(
            &mut self.output,
            r##"
    pub mod ports {{"##
        )?;

        if self.artifact == Artifact::Standalone {
            //
            // For the standalone build, we generate a mock port.
            //
            writeln!(
                &mut self.output,
                r##"
        #[allow(dead_code)]
        pub const fn i2c_mock() -> drv_i2c_api::PortIndex {{
            drv_i2c_api::PortIndex(0)
        }}"##
            )?;
        }

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

pub fn codegen(disposition: Disposition, artifact: Artifact) -> Result<()> {
    use std::io::Write;

    let out_dir = env::var("OUT_DIR")?;
    let dest_path = Path::new(&out_dir).join("i2c_config.rs");
    let mut file = File::create(&dest_path)?;

    let mut g = ConfigGenerator::new(disposition, artifact);

    g.generate_header()?;

    match disposition {
        Disposition::Target => {
            let n = g.ncontrollers();

            if n != 1 && artifact == Artifact::Dist {
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
    }

    g.generate_footer()?;

    file.write_all(g.output.as_bytes())?;

    Ok(())
}
