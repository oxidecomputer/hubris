use std::env;
use std::fmt::Write;
use std::fs::File;
use std::path::Path;
use anyhow::{bail, Result};

fn output_header() -> Result<String> {
    let mut s = String::new();
    writeln!(&mut s, r##"mod config {{
    #[cfg(feature = "h7b3")]
    use stm32h7::stm32h7b3 as device;

    #[cfg(feature = "h743")]
    use stm32h7::stm32h743 as device;

    use drv_i2c_api::{{Controller, Port}};

    #[allow(unused_imports)]
    use drv_i2c_api::Mux;

    use drv_stm32h7_gpio_api::{{self as gpio_api, Alternate}};
    use drv_stm32h7_i2c::{{I2cController, I2cMux, I2cPin}};
    use drv_stm32h7_rcc_api::Peripheral;
"##)?;

    Ok(s)
}

fn output_footer() -> Result<String> {
    let mut s = String::new();
    writeln!(&mut s, "}}")?;
    Ok(s)
}

fn output_controllers(i2c: &build_util::I2cConfig) -> Result<String> {
    let mut s = String::new();

    write!(&mut s,
        r##"    pub fn controllers() -> [I2cController<'static>; {}] {{
        ["##, i2c.controllers.len())?;

    for c in &i2c.controllers {
        write!(&mut s, r##"
            I2cController {{
                controller: Controller::I2C{controller},
                peripheral: Peripheral::I2c{controller},
                notification: (1 << ({controller} - 1)),
                registers: unsafe {{ &*device::I2C{controller}::ptr() }},
            }},"##, controller = c.controller)?;
    }

    writeln!(&mut s, r##"
        ]
    }}"##)?;

    Ok(s)
}

fn output_pins(i2c: &build_util::I2cConfig) -> Result<String> {
    let mut s = String::new();
    let mut len = 0;

    for c in &i2c.controllers {
        for (_, port) in &c.ports {
            len += port.pins.len();
        }
    }

    write!(&mut s, r##"
    pub fn pins() -> [I2cPin; {}] {{
        ["##, len)?;

    for c in &i2c.controllers {
        for (p, port) in &c.ports {
            for pin in &port.pins {
                let mut pinstr = String::new();
                write!(&mut pinstr, "pin({})", pin.pins[0])?;

                for i in 1..pin.pins.len() {
                    write!(&mut pinstr, ".and_pin({})", pin.pins[i])?;
                }

                write!(
                    &mut s, r##"
            I2cPin {{
                controller: Controller::I2C{controller},
                port: Port::{i2c_port},
                gpio_pins: gpio_api::Port::{gpio_port}.{pinstr},
                function: Alternate::AF{af},
            }},"##,
                    controller = c.controller, i2c_port = p,
                    gpio_port = match pin.port {
                        Some(ref port) => port,
                        None => p
                    },
                    pinstr = pinstr, af = pin.af
                )?;
            }
        }
    }

    writeln!(&mut s, r##"
        ]
    }}"##)?;

    Ok(s)
}

fn output_muxes(i2c: &build_util::I2cConfig) -> Result<String> {
    let mut s = String::new();
    let mut len = 0;

    for c in &i2c.controllers {
        for (_, port) in &c.ports {
            if let Some(ref muxes) = port.muxes {
                len += muxes.len();
            }
        }
    }

    write!(&mut s, r##"
    pub fn muxes() -> [I2cMux<'static>; {}] {{
        ["##, len)?;

    for c in &i2c.controllers {
        for (p, port) in &c.ports {
            if let Some(ref muxes) = port.muxes {
                for i in 0..muxes.len() {
                    let mux = &muxes[i];

                    let enablestr = if let Some(enable) = &mux.enable {
                        let mut enablestr = String::new();
                        write!(
                            &mut enablestr, r##"Some(I2cPin {{
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
                                    c.controller, p, i + 1
                                )
                            },
                            gpio_pin = enable.pins[0], af = enable.af
                        )?;
                        enablestr
                    } else {
                        "None".to_string()
                    };

                    let driver_struct =
                        format!(
                            "{}{}",
                            (&mux.driver[..1].to_string()).to_uppercase(),
                            &mux.driver[1..]
                        );

                    write!(
                        &mut s, r##"
            I2cMux {{
                controller: Controller::I2C{controller},
                port: Port::{i2c_port},
                id: Mux::M{ndx},
                driver: &drv_stm32h7_i2c::{driver}::{driver_struct},
                enable: {enable},
                address: 0x{address:x},
            }},"##,
                        controller = c.controller, i2c_port = p,
                        ndx = i + 1, driver = mux.driver,
                        driver_struct = driver_struct,
                        enable = enablestr, address = mux.address,
                    )?;
                }
            }
        }
    }

    writeln!(&mut s, r##"
        ]
    }}"##)?;

    Ok(s)
}

fn codegen() -> Result<()> {
    use std::io::Write;

    let config = build_util::i2c_config();

    let out_dir = env::var("OUT_DIR")?;
    let dest_path = Path::new(&out_dir).join("config.rs");
    let mut file = File::create(&dest_path)?;

    file.write_all(output_header()?.as_bytes())?;

    let out = output_controllers(&config)?;
    file.write_all(out.as_bytes())?;

    let out = output_pins(&config)?;
    file.write_all(out.as_bytes())?;

    let out = output_muxes(&config)?;
    file.write_all(out.as_bytes())?;

    file.write_all(output_footer()?.as_bytes())?;

    Ok(())
}

fn main() {
    build_util::expose_target_board();

    if let Err(e) = codegen() {
        println!("code generation failed: {}", e);
        std::process::exit(1);
    }

    println!("cargo:rerun-if-changed=build.rs");
}
