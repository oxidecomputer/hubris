use anyhow::{bail, Result};
use indexmap::IndexMap;
use multimap::MultiMap;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Write;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cPin {
    gpio_port: Option<String>,
    pins: Vec<u8>,
    af: u8,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cMux {
    driver: String,
    address: u8,
    enable: Option<I2cPin>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cPort {
    name: Option<String>,
    description: Option<String>,
    pins: Vec<I2cPin>,
    muxes: Option<Vec<I2cMux>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cController {
    controller: u8,
    ports: IndexMap<String, I2cPort>,
    target: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cPmbus {
    rails: Option<Vec<String>>,
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
    removable: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cConfig {
    controllers: Vec<I2cController>,
    devices: Option<Vec<I2cDevice>>,
}

#[derive(Clone, Debug, Deserialize)]
struct Config {
    i2c: I2cConfig,
}

#[derive(Copy, Clone, PartialEq)]
pub enum I2cConfigDisposition {
    /// controller is an initiator
    Initiator,
    /// controller is a target
    Target,
    /// controller is not used
    NoController,
    /// standalone build: config should be mocked
    Standalone,
}

#[allow(dead_code)]
pub struct I2cConfigGenerator {
    pub output: String,
    disposition: I2cConfigDisposition,
    controllers: Vec<I2cController>,
    devices: Vec<I2cDevice>,
    buses: HashMap<String, (u8, String)>,
    singletons: HashMap<u8, String>,
}

impl I2cConfigGenerator {
    pub fn new(disposition: I2cConfigDisposition) -> I2cConfigGenerator {
        let i2c = match disposition {
            I2cConfigDisposition::Standalone => I2cConfig {
                controllers: vec![],
                devices: None,
            },
            _ => match build_util::config::<Config>() {
                Ok(config) => config.i2c,
                Err(err) => {
                    panic!("malformed config.i2c: {:?}", err);
                }
            },
        };

        let mut controllers = vec![];
        let mut buses = HashMap::new();
        let mut singletons = HashMap::new();

        for c in i2c.controllers {
            let target = match c.target {
                Some(target) => target,
                None => false,
            };

            //
            // We always insert our buses (even for controllers that don't
            // match our dispostion) to assure that devices can always find
            // their bus.
            //
            for (p, port) in &c.ports {
                if let Some(name) = &port.name {
                    match buses.insert(name.clone(), (c.controller, p.clone()))
                    {
                        Some(_) => {
                            panic!("i2c bus {} appears twice", name);
                        }
                        None => {}
                    }
                }

                if c.ports.len() == 1 {
                    singletons.insert(c.controller, p.clone());
                }
            }

            if target != (disposition == I2cConfigDisposition::Target) {
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

        I2cConfigGenerator {
            output: String::new(),
            disposition: disposition,
            controllers: controllers,
            buses: buses,
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

        assert!(self.disposition != I2cConfigDisposition::NoController);

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

        #[cfg(feature = "h7b3")]
        use stm32h7::stm32h7b3 as device;

        #[cfg(feature = "h743")]
        use stm32h7::stm32h743 as device;"##
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

        assert!(self.disposition != I2cConfigDisposition::NoController);

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
        use drv_i2c_api::{{Controller, Port}};
        use drv_stm32h7_gpio_api::{{self as gpio_api, Alternate}};"##
            )?;
        }

        write!(
            &mut s,
            r##"
        ["##
        )?;

        for c in &self.controllers {
            for (p, port) in &c.ports {
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
                port: Port::{i2c_port},
                gpio_pins: gpio_api::Port::{gpio_port}.{pinstr},
                function: Alternate::AF{af},
            }},"##,
                        controller = c.controller,
                        i2c_port = p,
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
        if self.disposition == I2cConfigDisposition::Target {
            panic!("cannot generate muxes when configured as target");
        }

        let mut s = &mut self.output;
        let mut len = 0;

        for c in &self.controllers {
            for (_, port) in &c.ports {
                if let Some(ref muxes) = port.muxes {
                    len += muxes.len();
                }
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
        use drv_i2c_api::{{Controller, Port, Mux}};
        use drv_stm32h7_gpio_api::{{self as gpio_api, Alternate}};"##
            )?;
        }

        write!(
            &mut s,
            r##"
        ["##
        )?;

        for c in &self.controllers {
            for (p, port) in &c.ports {
                if let Some(ref muxes) = port.muxes {
                    for i in 0..muxes.len() {
                        let mux = &muxes[i];

                        let enablestr = if let Some(enable) = &mux.enable {
                            let mut enablestr = String::new();
                            write!(
                                &mut enablestr,
                                r##"Some(I2cPin {{
                    controller: Controller::I2C{controller},
                    port: Port::{port},
                    gpio_pins: gpio_api::Port::{gpio_port}.pin({gpio_pin}),
                    function: Alternate::AF{af},
                }})"##,
                                controller = c.controller,
                                port = p,
                                gpio_port = match enable.gpio_port {
                                    Some(ref port) => port,
                                    None => bail!(
                                        "missing pin port on mux enable \
                                        on I2C{}, port {}, mux {}",
                                        c.controller,
                                        p,
                                        i + 1
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
                port: Port::{i2c_port},
                id: Mux::M{ndx},
                driver: &drv_stm32h7_i2c::{driver}::{driver_struct},
                enable: {enable},
                address: 0x{address:x},
            }},"##,
                            controller = c.controller,
                            i2c_port = p,
                            ndx = i + 1,
                            driver = mux.driver,
                            driver_struct = driver_struct,
                            enable = enablestr,
                            address = mux.address,
                        )?;
                    }
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

        let port = match &d.bus {
            Some(bus) => &self.buses.get(bus).unwrap().1,
            None => match &d.port {
                Some(port) => port,
                None => match self.singletons.get(&d.controller.unwrap()) {
                    Some(port) => port,
                    None => {
                        panic!("device {} has ambiguous port", d.device)
                    }
                },
            },
        };

        format!(
            r##"
            // {description}
            I2cDevice::new(task,
                Controller::I2C{controller},
                Port::{port},
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
        if self.disposition == I2cConfigDisposition::Standalone {
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
        use drv_i2c_api::{{I2cDevice, Controller, Port}};
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
}
