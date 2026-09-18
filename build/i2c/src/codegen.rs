// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Stage 3: code generation.
//!
//! Each section is produced as a [`TokenStream`] via [`quote!`]; the caller
//! assembles the sections into the `i2c_config` module (see
//! [`i2c_config_module`]) and renders it to a string.

use crate::CodegenTarget;
use crate::analysis::{Device, DeviceSensor, Report, Validation};
use crate::load::{I2cDevice, I2cSensors, Sensor};
use anyhow::{Result, bail};
use convert_case::{Case, Casing};
use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::{format_ident, quote};
use rangemap::RangeSet;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Code generation for a single analyzed configuration.
///
/// Every method here is a pure function of the [`Report`] (plus the
/// code-generation-only settings in this struct): none of them validate
/// anything.
pub struct Codegen<'a> {
    /// The analyzed configuration.
    pub report: &'a Report,

    /// The chip we are generating code for.
    pub codegen_target: CodegenTarget,
}

#[derive(PartialEq)]
enum PowerDevices {
    /// PMBus power devices
    PMBus,

    /// Non-PMBus power devices
    NonPMBus,
}

/// Wraps generated sections in the `i2c_config` module that tasks
/// `include!`.
pub fn i2c_config_module(body: TokenStream) -> TokenStream {
    quote! {
        pub(crate) mod i2c_config {
            #body
        }
    }
}

/// Builds an identifier from a string.
///
/// The manifest is the source of every name used here (device names, bus
/// names, rail names, ...), and those names are only ever valid if they form
/// legal identifiers; a bad one is reported by `Ident::new`.
fn ident(s: &str) -> Ident {
    Ident::new(s, Span::call_site())
}

/// A device description as a doc attribute value.
///
/// The leading space matches what `/// text` desugars to.
fn doc(description: &str) -> String {
    format!(" {description}")
}

/// An unsuffixed `usize` literal (`3`, not `3usize`).
fn usize_lit(n: usize) -> Literal {
    Literal::usize_unsuffixed(n)
}

/// An unsuffixed `u8` literal (`3`, not `3u8`).
fn u8_lit(n: u8) -> Literal {
    Literal::u8_unsuffixed(n)
}

/// An unsuffixed hexadecimal literal (`0x48`), as used for I2C addresses.
fn hex_lit(n: u8) -> Literal {
    format!("{n:#x}")
        .parse()
        .expect("a formatted hex integer is always a valid literal")
}

impl Codegen<'_> {
    pub fn generate_controllers(&self) -> Result<TokenStream> {
        let ncontrollers = usize_lit(self.report.controllers.len());

        let imports = if self.report.controllers.is_empty() {
            quote!()
        } else {
            let device = match self.codegen_target {
                CodegenTarget::None => quote!(),
                CodegenTarget::Stm32H743 => {
                    quote!(
                        use stm32h7::stm32h743 as device;
                    )
                }
                CodegenTarget::Stm32H753 => {
                    quote!(
                        use stm32h7::stm32h753 as device;
                    )
                }
                CodegenTarget::Stm32G031 => {
                    quote!(
                        use stm32g0::stm32g031 as device;
                    )
                }
                CodegenTarget::Stm32G030 => {
                    quote!(
                        use stm32g0::stm32g030 as device;
                    )
                }
            };
            quote! {
                use drv_stm32xx_sys_api::Peripheral;
                use drv_i2c_api::Controller;
                #device
            }
        };

        let controllers = self.report.controllers.iter().map(|c| {
            let controller = format_ident!("I2C{}", c.controller);
            let peripheral = format_ident!("I2c{}", c.controller);
            let irq_mask = format_ident!("I2C{}_IRQ_MASK", c.controller);
            quote! {
                I2cController {
                    controller: Controller::#controller,
                    peripheral: Peripheral::#peripheral,
                    notification: crate::notifications::#irq_mask,
                    registers: unsafe { &*device::#controller::ptr() },
                }
            }
        });

        Ok(quote! {
            #[allow(dead_code)]
            pub const NCONTROLLERS: usize = #ncontrollers;

            use drv_stm32xx_i2c::I2cController;

            pub fn controllers() -> [I2cController<'static>; NCONTROLLERS] {
                #imports
                [ #(#controllers),* ]
            }
        })
    }

    pub fn generate_pins(&self) -> Result<TokenStream> {
        let len = self
            .report
            .controllers
            .iter()
            .map(|c| c.ports.len())
            .sum::<usize>();

        let imports = if len > 0 {
            quote! {
                use drv_i2c_api::{Controller, PortIndex};
                use drv_stm32xx_sys_api::{self as gpio_api, Alternate};
            }
        } else {
            quote!()
        };

        let pins = self.report.controllers.iter().flat_map(|c| {
            c.ports.iter().enumerate().map(move |(index, (p, port))| {
                let controller = format_ident!("I2C{}", c.controller);
                let index = usize_lit(index);
                let scl = ident(port.scl.gpio_port.as_deref().unwrap_or(p));
                let scl_pin = u8_lit(port.scl.pin);
                let sda = ident(port.sda.gpio_port.as_deref().unwrap_or(p));
                let sda_pin = u8_lit(port.sda.pin);
                let af = format_ident!("AF{}", port.af);
                quote! {
                    I2cPins {
                        controller: Controller::#controller,
                        port: PortIndex(#index),
                        scl: gpio_api::Port::#scl.pin(#scl_pin),
                        sda: gpio_api::Port::#sda.pin(#sda_pin),
                        function: Alternate::#af,
                    }
                }
            })
        });

        let len = usize_lit(len);

        Ok(quote! {
            #[allow(unused_imports)]
            use drv_stm32xx_i2c::{I2cPins, I2cGpio};

            pub fn pins() -> [I2cPins; #len] {
                #imports
                [ #(#pins),* ]
            }
        })
    }

    pub fn generate_muxes(&self) -> Result<TokenStream> {
        let mut nmuxedbuses = 0;
        let mut len = 0;

        for c in &self.report.controllers {
            for port in c.ports.values() {
                if !port.muxes.is_empty() {
                    nmuxedbuses += 1;
                }

                len += port.muxes.len();
            }
        }

        let imports = if len > 0 {
            quote! {
                use drv_i2c_api::{Controller, PortIndex, Mux};

                #[allow(unused_imports)]
                use drv_stm32xx_sys_api::{self as gpio_api, Alternate};
            }
        } else {
            quote!()
        };

        let muxes = self.report.controllers.iter().flat_map(|c| {
            c.ports.values().enumerate().flat_map(move |(index, port)| {
                port.muxes.iter().enumerate().map(move |(mindex, mux)| {
                    let controller = format_ident!("I2C{}", c.controller);
                    let i2c_port = usize_lit(index);
                    let id = format_ident!("M{}", mindex + 1);
                    let driver = ident(&mux.driver);
                    let driver_struct = ident(&format!(
                        "{}{}",
                        mux.driver[..1].to_uppercase(),
                        &mux.driver[1..]
                    ));
                    let nreset = match &mux.nreset {
                        Some(enable) => {
                            let gpio_port = ident(&enable.port);
                            let gpio_pin = u8_lit(enable.pin);
                            quote! {
                                Some(I2cGpio {
                                    gpio_pins: gpio_api::Port::#gpio_port.pin(#gpio_pin),
                                })
                            }
                        }
                        None => quote!(None),
                    };
                    let address = hex_lit(mux.address);

                    quote! {
                        I2cMux {
                            controller: Controller::#controller,
                            port: PortIndex(#i2c_port),
                            id: Mux::#id,
                            driver: &drv_stm32xx_i2c::#driver::#driver_struct,
                            nreset: #nreset,
                            address: #address,
                        }
                    }
                })
            })
        });

        let nmuxedbuses = usize_lit(nmuxedbuses);
        let len = usize_lit(len);

        Ok(quote! {
            #[allow(dead_code)]
            pub const NMUXEDBUSES: usize = #nmuxedbuses;

            use drv_stm32xx_i2c::I2cMux;

            pub fn muxes() -> [I2cMux<'static>; #len] {
                #imports
                [ #(#muxes),* ]
            }
        })
    }

    /// The expression constructing an `I2cDevice` handle for `d`.
    ///
    /// This expects `task: TaskId` to be in scope, along with `I2cDevice`,
    /// `Controller` and `PortIndex` from `drv_i2c_api`.
    fn device_expr(&self, d: &Device) -> TokenStream {
        let controller = format_ident!("I2C{}", d.controller);
        let port = usize_lit(d.port);

        let segment = match d.segment {
            Some((mux, segment)) => {
                let mux = format_ident!("M{mux}");
                let segment = format_ident!("S{segment}");
                quote!(Some((drv_i2c_api::Mux::#mux, drv_i2c_api::Segment::#segment)))
            }
            None => quote!(None),
        };

        let address = hex_lit(d.config.address);

        let component_id = d.component_id.as_ref().map(|id| quote!(, #id));

        quote! {
            I2cDevice::new(
                task,
                Controller::#controller,
                PortIndex(#port),
                #segment,
                #address
                #component_id
            )
        }
    }

    /// A function returning an array of every device in `indices`.
    fn device_array_fn(&self, name: &str, indices: &[usize]) -> TokenStream {
        let name = ident(name);
        let len = usize_lit(indices.len());
        let devices = indices.iter().map(|&i| &self.report.devices[i]);
        let docs = devices.clone().map(|d| doc(&d.config.description));
        let exprs = devices.map(|d| self.device_expr(d));

        quote! {
            #(#[doc = #docs])*
            #[allow(dead_code)]
            pub fn #name(task: TaskId) -> [I2cDevice; #len] {
                [ #(#exprs),* ]
            }
        }
    }

    /// A function returning the single device at `index`.
    fn device_fn(&self, name: &str, index: usize) -> TokenStream {
        let name = ident(name);
        let device = &self.report.devices[index];
        let doc = doc(&device.config.description);
        let expr = self.device_expr(device);

        quote! {
            #[doc = #doc]
            #[allow(dead_code)]
            pub fn #name(task: TaskId) -> I2cDevice {
                #expr
            }
        }
    }

    pub fn generate_devices(&self) -> Result<TokenStream> {
        //
        // Generate a function that looks up an `I2cDevice` based on its index
        // in the order returned by `device_descriptions()`.
        //
        // This is used by the generated code in `task-validate-api` and
        // `control-plane-agent`, such as when we construct an `I2cDevice handle
        // in order to read VPD or PMBus registers from a device. These indices
        // are also referenced by the lookup table of PMBus rail names to
        // device indices in `control-plane-agent`.
        //
        let task_arg = if self.report.devices.is_empty() {
            // If we are generating a `device_by_index` function that has no
            // devices in it, this argument will be unused, so suppress clippy
            // warnings about it.
            ident("_task")
        } else {
            ident("task")
        };

        let by_index =
            self.report.devices.iter().enumerate().map(|(index, d)| {
                let index = usize_lit(index);
                let doc = doc(&d.config.description);
                let expr = self.device_expr(d);
                quote! {
                    #[doc = #doc]
                    #index => Some(#expr),
                }
            });

        let by_controller = match_arms(&self.report.by_controller, |c| {
            let c = format_ident!("I2C{c}");
            quote!(Some(Controller::#c))
        });

        let by_port = match_arms(&self.report.by_port, |p| {
            let p = usize_lit(*p);
            quote!(Some(PortIndex(#p)))
        });

        let by_device = self
            .report
            .by_device
            .iter()
            .map(|(device, indices)| self.device_array_fn(device, indices));

        let by_bus =
            self.report.by_bus.iter().map(|((device, bus), indices)| {
                self.device_array_fn(&format!("{device}_{bus}"), indices)
            });

        let by_name =
            self.report.by_name.iter().map(|((device, name), index)| {
                self.device_fn(
                    &format!("{device}_{}", name.to_lowercase()),
                    *index,
                )
            });

        let by_refdes =
            self.report
                .by_refdes
                .iter()
                .map(|((device, refdes), index)| {
                    self.device_fn(
                        &format!("{device}_{}", refdes.to_lower_ident()),
                        *index,
                    )
                });

        let max_component_id_len = self.report.component_ids.then(|| {
            let len = usize_lit(self.report.max_component_id_len);
            quote! {
                #[allow(dead_code)]
                pub const MAX_COMPONENT_ID_LEN: usize = #len;
            }
        });

        let pmbus = self.generate_power(PowerDevices::PMBus);
        let power = self.generate_power(PowerDevices::NonPMBus);

        Ok(quote! {
            pub mod devices {
                #[allow(unused_imports)]
                use drv_i2c_api::{I2cDevice, Controller, PortIndex};
                #[allow(unused_imports)]
                use userlib::TaskId;

                #[allow(dead_code)]
                #[allow(clippy::match_single_binding)]
                #[allow(unused_doc_comments)]
                pub fn device_by_index(
                    #task_arg: TaskId,
                    index: usize,
                ) -> Option<I2cDevice> {
                    match index {
                        #(#by_index)*
                        _ => None,
                    }
                }

                #[allow(dead_code)]
                #[allow(clippy::match_single_binding)]
                pub fn lookup_controller(index: usize) -> Option<Controller> {
                    match index {
                        #by_controller
                        _ => None
                    }
                }

                #[allow(dead_code)]
                #[allow(clippy::match_single_binding)]
                pub fn lookup_port(index: usize) -> Option<PortIndex> {
                    match index {
                        #by_port
                        _ => None
                    }
                }

                #(#by_device)*
                #(#by_bus)*
                #(#by_name)*
                #(#by_refdes)*
            }

            #max_component_id_len

            #pmbus
            #power
        })
    }

    pub fn generate_validation(&self) -> Result<TokenStream> {
        let Some(validation) = &self.report.validation else {
            bail!(
                "internal error: validation code generation was requested, \
                 but validation was not analyzed"
            );
        };

        // The ordering / index values of this `match` must match the ordering
        // returned by `device_descriptions()`: if we change the ordering here,
        // it must be updated there as well.
        let arms = self.report.devices.iter().enumerate().map(|(index, d)| {
            let doc = doc(&d.config.description);
            let expr = self.device_expr(d);
            let strategy = &validation[index];
            let index = usize_lit(index);

            match strategy {
                Validation::Driver(driver) => {
                    let module = ident(&d.config.device);
                    let driver = ident(driver);
                    quote! {
                        #[doc = #doc]
                        #index => {
                            if drv_i2c_devices::#module::#driver::validate(&#expr)? {
                                Ok(I2cValidation::Good)
                            } else {
                                Ok(I2cValidation::Bad)
                            }
                        }
                    }
                }
                Validation::RawRead => quote! {
                    #[doc = #doc]
                    #index => {
                        #expr.read::<u8>()?;
                        Ok(I2cValidation::RawReadOk)
                    }
                },
            }
        });

        Ok(quote! {
            pub mod validation {
                #[allow(unused_imports)]
                use drv_i2c_api::{I2cDevice, Controller, PortIndex};
                #[allow(unused_imports)]
                use drv_i2c_devices::Validate;
                use userlib::TaskId;

                #[allow(dead_code)]
                pub enum I2cValidation {
                    RawReadOk,
                    Good,
                    Bad,
                }

                #[allow(unused_variables)]
                #[allow(clippy::match_single_binding)]
                #[allow(unused_doc_comments)]
                pub fn validate(
                    task: TaskId,
                    index: usize,
                ) -> Result<I2cValidation, drv_i2c_api::ResponseCode> {
                    match index {
                        #(#arms)*
                        _ => Err(drv_i2c_api::ResponseCode::BadArg)
                    }
                }
            }
        })
    }

    fn generate_power(&self, which: PowerDevices) -> TokenStream {
        let rails = match which {
            PowerDevices::PMBus => &self.report.pmbus_rails,
            PowerDevices::NonPMBus => &self.report.power_rails,
        };

        if rails.is_empty() {
            return quote!();
        }

        let module = match which {
            PowerDevices::PMBus => ident("pmbus"),
            PowerDevices::NonPMBus => ident("power"),
        };

        let items = rails.iter().map(|entry| {
            let rail = &entry.rail;
            let device = &self.report.devices[entry.device];

            // Accessor, returns `(I2cDevice, Option<u8>)`
            let name = ident(&rail.to_lowercase());
            let doc = doc(&device.config.description);
            let expr = self.device_expr(device);
            let bank = match entry.bank {
                Some(idx) => {
                    let idx = usize_lit(idx);
                    quote!(Some(#idx))
                }
                None => quote!(None),
            };

            let accessor = quote! {
                #[doc = #doc]
                #[allow(dead_code)]
                pub fn #name(task: TaskId) -> (I2cDevice, Option<u8>) {
                    (#expr, #bank)
                }
            };

            let phases = (which == PowerDevices::PMBus).then(|| {
                let phases = match &entry.phases {
                    Some(phases) => {
                        let phases = phases.iter().map(|&p| u8_lit(p));
                        quote!(Some(&[#(#phases),*]))
                    }
                    None => quote!(None),
                };
                let name = ident(&format!(
                    "{}_{rail}_PHASES",
                    device.config.device.to_uppercase()
                ));
                quote! {
                    #[allow(dead_code)]
                    pub const #name: Option<&'static [u8]> = #phases;
                }
            });

            quote! {
                #accessor
                #phases
            }
        });

        quote! {
            pub mod #module {
                use drv_i2c_api::{I2cDevice, Controller, PortIndex};
                use userlib::TaskId;

                #(#items)*
            }
        }
    }

    /// Emits the `NUM_*_SENSORS` count and `*_SENSOR`/`*_SENSORS` ID
    /// constants for one group of sensors, optionally documented with `doc`.
    fn emit_sensor(
        &self,
        device: &str,
        label: &str,
        ids: &[usize],
        doc: Option<&str>,
    ) -> TokenStream {
        let device = device.to_uppercase();
        let n_sensors = ids.len();
        let doc = doc.map(|d| {
            let d = self::doc(d);
            quote!(#[doc = #d])
        });

        let count = ident(&format!("NUM_{device}_{label}_SENSORS"));
        let n = usize_lit(n_sensors);

        let sensors = if let [id] = ids {
            let name = ident(&format!("{device}_{label}_SENSOR"));
            let id = usize_lit(*id);
            quote! {
                #doc
                #[allow(dead_code)]
                pub const #name: SensorId = SensorId::new(#id);
            }
        } else {
            let name = ident(&format!("{device}_{label}_SENSORS"));
            let ids = ids.iter().map(|&id| usize_lit(id));
            quote! {
                #doc
                #[allow(dead_code)]
                pub const #name: [SensorId; #n] = [ #(SensorId::new(#ids)),* ];
            }
        };

        quote! {
            #[allow(dead_code)]
            pub const #count: usize = #n;

            #sensors
        }
    }

    fn declare_sensor_struct(
        &self,
        d: &I2cDevice,
        struct_name: &str,
    ) -> TokenStream {
        let name = ident(&format!("Sensors_{struct_name}"));

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
            let fields = [
                ("temperature", *temperature),
                ("power", *power),
                ("current", *current),
                ("voltage", *voltage),
                ("input_current", *input_current),
                ("input_voltage", *input_voltage),
                ("speed", *speed),
            ]
            .into_iter()
            .filter_map(|(field, count)| {
                let field = ident(field);
                match count {
                    0 => None,
                    1 => Some(quote!(pub #field: SensorId,)),
                    _ => {
                        let count = usize_lit(count);
                        Some(quote!(pub #field: [SensorId; #count],))
                    }
                }
            });

            quote! {
                #[allow(non_camel_case_types, dead_code)]
                pub struct #name {
                    #(#fields)*
                }
            }
        } else {
            quote! {
                #[allow(dead_code, non_camel_case_types)]
                type #name = ();
            }
        }
    }

    fn emit_sensor_struct(
        &self,
        d: &I2cDevice,
        label: &str,
        name: &str,
        sensors: &[Arc<DeviceSensor>],
    ) -> TokenStream {
        let const_name =
            ident(&format!("{}_{label}_SENSORS", d.device.to_uppercase()));
        let ty = ident(&format!("Sensors_{name}"));

        let mut sensors_by_kind: BTreeMap<Sensor, Vec<usize>> = BTreeMap::new();
        for s in sensors {
            sensors_by_kind.entry(s.kind).or_default().push(s.id);
        }

        if sensors_by_kind.is_empty() {
            return quote! {
                #[allow(dead_code)]
                pub const #const_name: #ty = ();
            };
        }

        let fields = sensors_by_kind.into_iter().map(|(kind, values)| {
            let field = ident(match kind {
                Sensor::Temperature => "temperature",
                Sensor::Power => "power",
                Sensor::Current => "current",
                Sensor::Voltage => "voltage",
                Sensor::InputCurrent => "input_current",
                Sensor::InputVoltage => "input_voltage",
                Sensor::Speed => "speed",
                Sensor::Pwm => "pwm",
            });
            let value = if let [v] = values.as_slice() {
                let v = usize_lit(*v);
                quote!(SensorId::new(#v))
            } else {
                let values = values.iter().map(|&v| usize_lit(v));
                quote!([ #(SensorId::new(#values)),* ])
            };
            quote!(#field: #value,)
        });

        quote! {
            #[allow(dead_code)]
            pub const #const_name: #ty = #ty {
                #(#fields)*
            };
        }
    }

    pub fn generate_sensors(&self) -> Result<TokenStream> {
        let s = &self.report.sensors;
        let total = usize_lit(s.total_i2c_sensors);

        let structs = self.report.devices.iter().enumerate().map(|(i, d)| {
            let info = &self.report.sensor_structs[i];

            let declaration = info
                .declare
                .then(|| self.declare_sensor_struct(&d.config, &info.name));

            let sensors = s.device_sensors[i].as_slice();

            let consts = info.labels.iter().map(|label| {
                self.emit_sensor_struct(&d.config, label, &info.name, sensors)
            });

            quote! {
                #declaration
                #(#consts)*
            }
        });

        let by_device = s.by_device.iter().map(|(k, ids)| {
            self.emit_sensor(&k.device, &k.kind.to_string(), ids, None)
        });

        let by_name = s.by_name.iter().map(|(k, ids)| {
            let label = format!("{}_{}", k.name.to_uppercase(), k.kind);
            self.emit_sensor(&k.device, &label, ids, None)
        });

        let by_refdes = s.by_refdes.iter().map(|(k, ids)| {
            let label = format!("{}_{}", k.refdes.to_upper_ident(), k.kind);
            self.emit_sensor(&k.device, &label, ids, None)
        });

        Ok(quote! {
            pub mod sensors {
                #[allow(unused_imports)]
                use super::super::SensorId;

                #[allow(dead_code)]
                pub const NUM_SENSORS: usize = #total;

                #(#structs)*
                #(#by_device)*
                #(#by_name)*
                #(#by_refdes)*
            }
        })
    }

    /// The `other_sensors` module: constants for the sensors of non-I2C
    /// devices (from `[config.sensor]`), which share the ID space with the
    /// I2C sensors.
    pub fn generate_other_sensors(&self) -> Result<TokenStream> {
        let s = &self.report.sensors;
        let total = usize_lit(s.total_other_sensors);

        let consts = s.other_sensors.iter().flat_map(|d| {
            d.ids_by_kind.iter().map(|(kind, ids)| {
                let label =
                    format!("{}_{kind}", d.config.name.to_ascii_uppercase());
                self.emit_sensor(
                    &d.config.device,
                    &label,
                    ids,
                    Some(&d.config.description),
                )
            })
        });

        Ok(quote! {
            pub mod other_sensors {
                #[allow(unused_imports)]
                use super::super::SensorId;

                #[allow(dead_code)]
                pub const NUM_SENSORS: usize = #total;

                #(#consts)*
            }
        })
    }

    /// A table mapping every sensor ID (I2C and otherwise) to its component
    /// ID, as `fixedstr::FixedStr`s.
    ///
    /// This is an error if any sensor has no refdes.
    pub fn generate_sensor_id_to_component_id(&self) -> Result<TokenStream> {
        let mut ids = Vec::new();
        let mut max_len = 0;

        for sensor in &self.report.sensors.by_id {
            let Some(refdes) = &sensor.refdes else {
                bail!(
                    "we were asked to generate a sensor-ID-to-component-ID \
                     lookup table, but sensor ID {} (name: {:?}, type: {:?}) \
                     has no refdes",
                    sensor.id,
                    sensor.name.as_deref().unwrap_or("<no name>"),
                    sensor.kind,
                );
            };
            let cid = refdes.to_component_id();
            max_len = max_len.max(cid.len());
            ids.push(cid);
        }

        let n = usize_lit(ids.len());
        let max_len = usize_lit(max_len);

        Ok(quote! {
            pub const MAX_SENSOR_COMPONENT_ID_LEN: usize = #max_len;

            pub const SENSOR_ID_TO_COMPONENT_ID: [
                fixedstr::FixedStr<'static, MAX_SENSOR_COMPONENT_ID_LEN>;
                #n
            ] = [ #(fixedstr::FixedStr::from_str(#ids)),* ];
        })
    }

    /// A table mapping every sensor ID (I2C and otherwise) to its name, as
    /// `fixedstr::FixedStr`s.
    ///
    /// This is an error if any sensor has no name.
    pub fn generate_sensor_id_to_name(&self) -> Result<TokenStream> {
        let mut names = Vec::new();
        let mut max_len = 0;

        for sensor in &self.report.sensors.by_id {
            let Some(name) = &sensor.name else {
                bail!(
                    "we were asked to generate a sensor-name lookup table, \
                     but sensor {sensor:?} has no name"
                );
            };
            max_len = max_len.max(name.len());
            names.push(name.clone());
        }

        let n = usize_lit(names.len());
        let max_len = usize_lit(max_len);

        Ok(quote! {
            pub const MAX_SENSOR_NAME_LEN: usize = #max_len;

            pub const SENSOR_ID_TO_NAME: [
                fixedstr::FixedStr<'static, MAX_SENSOR_NAME_LEN>;
                #n
            ] = [ #(fixedstr::FixedStr::from_str(#names)),* ];
        })
    }

    pub fn generate_ports(&self) -> Result<TokenStream> {
        let ports =
            self.report.ports.iter().map(|((controller, port), index)| {
                let name = ident(&format!(
                    "i2c{controller}_{}",
                    port.to_case(Case::Snake)
                ));
                let index = usize_lit(*index);
                quote! {
                    #[allow(dead_code)]
                    pub const fn #name() -> drv_i2c_api::PortIndex {
                        drv_i2c_api::PortIndex(#index)
                    }
                }
            });

        Ok(quote! {
            pub mod ports {
                #(#ports)*
            }
        })
    }
}

/// Generates `match` arms mapping each group of device indices to a result,
/// coalescing runs of consecutive indices into ranges (`0..=2 | 5..=5`).
fn match_arms<C>(
    source: &[(C, Vec<usize>)],
    result: impl Fn(&C) -> TokenStream,
) -> TokenStream {
    let arms = source.iter().map(|(key, indices)| {
        let ranges = indices
            .iter()
            .map(|&i| i..i + 1)
            .collect::<RangeSet<usize>>();
        let patterns = ranges.iter().map(|range| {
            let start = usize_lit(range.start);
            let end = usize_lit(range.end - 1);
            quote!(#start..=#end)
        });
        let result = result(key);
        quote! {
            #(#patterns)|* => #result,
        }
    });

    quote!(#(#arms)*)
}
