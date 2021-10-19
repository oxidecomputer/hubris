use anyhow::{bail, Result};
use indexmap::IndexMap;
use serde::Deserialize;
use std::fmt::Write;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct I2cPin {
    port: Option<String>,
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
struct I2cDevice {
    driver: String,
    controller: u8,
    address: u8,
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
    /// standalone build: config should be mocked
    Standalone,
}

#[allow(dead_code)]
pub struct I2cConfigGenerator {
    pub output: String,
    disposition: I2cConfigDisposition,
    controllers: Vec<I2cController>,
}

impl I2cConfigGenerator {
    pub fn new(disposition: I2cConfigDisposition) -> I2cConfigGenerator {
        let i2c = match disposition {
            I2cConfigDisposition::Standalone => {
                I2cConfig {
                    controllers: vec![],
                    devices: None,
                }
            }
            _ => {
                match build_util::config::<Config>() {
                    Ok(config) => config.i2c,
                    Err(err) => {
                        panic!("malformed config.i2c: {:?}", err);
                    }
                }
            }
        };

        let mut controllers = vec![];

        for c in i2c.controllers {
            let target = match c.target {
                Some(target) => target,
                None => false,
            };

            if target != (disposition == I2cConfigDisposition::Target) {
                continue;
            }

            controllers.push(c);
        }

        I2cConfigGenerator {
            output: String::new(),
            disposition: disposition,
            controllers: controllers,
        }
    }

    pub fn ncontrollers(&self) -> usize {
        self.controllers.len()
    }

    pub fn generate_header(&mut self) -> Result<()> {
        let mut s = &mut self.output;

        if self.disposition == I2cConfigDisposition::Standalone {
            writeln!(&mut s, r##"mod config {{
    use drv_stm32h7_i2c::{{I2cController, I2cPin}};

    #[allow(unused_imports)]
    use drv_stm32h7_i2c::I2cMux;"##)?;

            return Ok(());
        }

        writeln!(
            &mut s,
            r##"mod config {{
    #[cfg(feature = "h7b3")]
    use stm32h7::stm32h7b3 as device;

    #[cfg(feature = "h743")]
    use stm32h7::stm32h743 as device;

    use drv_i2c_api::{{Controller, Port}};

    #[allow(unused_imports)]
    use drv_i2c_api::Mux;

    use drv_stm32h7_gpio_api::{{self as gpio_api, Alternate}};
    use drv_stm32h7_i2c::{{I2cController, I2cPin}};

    #[allow(unused_imports)]
    use drv_stm32h7_i2c::I2cMux;

    use drv_stm32h7_rcc_api::Peripheral;
        "##
        )?;

        Ok(())
    }

    pub fn generate_footer(&mut self) -> Result<()> {
        writeln!(&mut self.output, "}}")?;
        Ok(())
    }

    pub fn generate_controllers(&mut self) -> Result<()> {
        let mut s = &mut self.output;

        write!(
            &mut s,
            r##"    pub fn controllers() -> [I2cController<'static>; {}] {{
        ["##,
            self.controllers.len()
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

        for c in &self.controllers {
            for (_, port) in &c.ports {
                len += port.pins.len();
            }
        }

        write!(
            &mut s,
            r##"
    pub fn pins() -> [I2cPin; {}] {{
        ["##,
            len
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
                        gpio_port = match pin.port {
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
    pub fn muxes() -> [I2cMux<'static>; {}] {{
        ["##,
            len
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
                    port: Port::Default,
                    gpio_pins: gpio_api::Port::{gpio_port}.pin({gpio_pin}),
                    function: Alternate::AF{af},
                }})"##,
                                controller = c.controller,
                                gpio_port = match enable.port {
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
}
