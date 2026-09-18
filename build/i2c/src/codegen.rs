// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Stage 3: code generation.

use crate::CodegenTarget;
use crate::analysis::{Device, DeviceSensor, Report, Validation};
use crate::load::{I2cDevice, I2cSensors, Sensor};
use anyhow::{Result, bail};
use convert_case::{Case, Casing};
use rangemap::RangeSet;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Arc;

/// Code generation for a single analyzed configuration.
///
/// Every method here is a pure function of the [`Report`] (plus the
/// code-generation-only settings in this struct): none of them validate
/// anything, and none of them panic.
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

impl Codegen<'_> {
    pub fn generate_header(&self, output: &mut String) -> Result<()> {
        writeln!(output, "pub(crate) mod i2c_config {{")?;
        Ok(())
    }

    pub fn generate_footer(&self, output: &mut String) -> Result<()> {
        writeln!(output, "}}")?;
        Ok(())
    }

    pub fn generate_controllers(&self, output: &mut String) -> Result<()> {
        writeln!(
            output,
            r##"
    #[allow(dead_code)]
    pub const NCONTROLLERS: usize = {ncontrollers};

    use drv_stm32xx_i2c::I2cController;

    pub fn controllers() -> [I2cController<'static>; NCONTROLLERS] {{"##,
            ncontrollers = self.report.controllers.len()
        )?;

        if !self.report.controllers.is_empty() {
            writeln!(
                output,
                r##"
        use drv_stm32xx_sys_api::Peripheral;
        use drv_i2c_api::Controller;"##
            )?;

            let text = match self.codegen_target {
                CodegenTarget::None => "",
                CodegenTarget::Stm32H743 => "use stm32h7::stm32h743 as device;",
                CodegenTarget::Stm32H753 => "use stm32h7::stm32h753 as device;",
                CodegenTarget::Stm32G031 => "use stm32g0::stm32g031 as device;",
                CodegenTarget::Stm32G030 => "use stm32g0::stm32g030 as device;",
            };
            writeln!(output, "{text}")?;
        }

        write!(
            output,
            r##"
        ["##
        )?;

        for c in &self.report.controllers {
            write!(
                output,
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
            output,
            r##"
        ]
    }}"##
        )?;

        Ok(())
    }

    pub fn generate_pins(&self, output: &mut String) -> Result<()> {
        let mut len = 0;

        for c in &self.report.controllers {
            len += c.ports.len();
        }

        writeln!(
            output,
            r##"
    #[allow(unused_imports)]
    use drv_stm32xx_i2c::{{I2cPins, I2cGpio}};

    pub fn pins() -> [I2cPins; {len}] {{"##,
        )?;

        if len > 0 {
            writeln!(
                output,
                r##"
        use drv_i2c_api::{{Controller, PortIndex}};
        use drv_stm32xx_sys_api::{{self as gpio_api, Alternate}};"##
            )?;
        }

        write!(
            output,
            r##"
        ["##
        )?;

        for c in &self.report.controllers {
            for (index, (p, port)) in c.ports.iter().enumerate() {
                writeln!(
                    output,
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
            output,
            r##"
        ]
    }}"##
        )?;

        Ok(())
    }

    pub fn generate_muxes(&self, output: &mut String) -> Result<()> {
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

        write!(
            output,
            r##"
    #[allow(dead_code)]
    pub const NMUXEDBUSES: usize = {nmuxedbuses};

    use drv_stm32xx_i2c::I2cMux;

    pub fn muxes() -> [I2cMux<'static>; {len}] {{"##,
        )?;

        if len > 0 {
            writeln!(
                output,
                r##"
        use drv_i2c_api::{{Controller, PortIndex, Mux}};

        #[allow(unused_imports)]
        use drv_stm32xx_sys_api::{{self as gpio_api, Alternate}};"##
            )?;
        }

        write!(
            output,
            r##"
        ["##
        )?;

        for c in &self.report.controllers {
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
                        output,
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
            output,
            r##"
        ]
    }}"##
        )?;

        Ok(())
    }

    fn generate_device(&self, d: &Device, indent: usize) -> String {
        let controller = d.controller;
        let port = d.port;

        let segment = match d.segment {
            Some((mux, segment)) => format!(
                "Some((drv_i2c_api::Mux::M{mux}, drv_i2c_api::Segment::S{segment}))",
            ),
            None => "None".to_owned(),
        };

        let indent = format!("{:indent$}", "", indent = indent);

        let component_id = match &d.component_id {
            Some(id) => format!("\n{indent}    {id:?},"),
            None => String::new(),
        };

        format!(
            r##"
{indent}// {description}
{indent}I2cDevice::new(task,
{indent}    Controller::I2C{controller},
{indent}    PortIndex({port}),
{indent}    {segment},
{indent}    {address:#x},{component_id}
{indent})"##,
            description = d.config.description,
            controller = controller,
            port = port,
            segment = segment,
            address = d.config.address,
            indent = indent,
        )
    }

    pub fn generate_devices(&self, output: &mut String) -> Result<()> {
        write!(
            output,
            r##"
    pub mod devices {{
        #[allow(unused_imports)]
        use drv_i2c_api::{{I2cDevice, Controller, PortIndex}};
        #[allow(unused_imports)]
        use userlib::TaskId;
"##
        )?;
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
            "_task"
        } else {
            "task"
        };
        write!(
            output,
            r##"
        #[allow(dead_code)]
        #[allow(clippy::match_single_binding)]
        pub fn device_by_index(
            {task_arg}: TaskId,
            index: usize,
        ) -> Option<I2cDevice> {{
            match index {{"##,
        )?;

        for (index, device) in self.report.devices.iter().enumerate() {
            let out = self.generate_device(device, 20);
            writeln!(output, "{index} => Some({out}),")?;
        }

        write!(
            output,
            r##"
                _ => None,
            }}
        }}

        #[allow(dead_code)]
        #[allow(clippy::match_single_binding)]
        pub fn lookup_controller(index: usize) -> Option<Controller> {{
            match index {{"##
        )?;

        match_arms(output, &self.report.by_controller, |c| {
            format!("Some(Controller::I2C{c})")
        })?;

        write!(
            output,
            r##"
                _ => None
            }}
        }}
"##
        )?;

        write!(
            output,
            r##"
        #[allow(dead_code)]
        #[allow(clippy::match_single_binding)]
        pub fn lookup_port(index: usize) -> Option<PortIndex> {{
            match index {{"##
        )?;

        match_arms(output, &self.report.by_port, |p| {
            format!("Some(PortIndex({p}))")
        })?;

        write!(
            output,
            r##"
                _ => None
            }}
        }}
"##
        )?;

        for (device, indices) in &self.report.by_device {
            write!(
                output,
                r##"
        #[allow(dead_code)]
        pub fn {}(task: TaskId) -> [I2cDevice; {}] {{
            ["##,
                device,
                indices.len()
            )?;

            for &i in indices {
                let out = self.generate_device(&self.report.devices[i], 16);
                write!(output, "{out},")?;
            }

            writeln!(
                output,
                r##"
            ]
        }}"##
            )?;
        }

        for ((device, bus), indices) in &self.report.by_bus {
            write!(
                output,
                r##"
        #[allow(dead_code)]
        pub fn {}_{}(task: TaskId) -> [I2cDevice; {}] {{
            ["##,
                device,
                bus,
                indices.len()
            )?;

            for &i in indices {
                let out = self.generate_device(&self.report.devices[i], 16);
                write!(output, "{out},")?;
            }
            writeln!(
                output,
                r##"
            ]
        }}"##
            )?;
        }

        for ((device, name), index) in &self.report.by_name {
            write!(
                output,
                r##"
        #[allow(dead_code)]
        pub fn {}_{}(task: TaskId) -> I2cDevice {{"##,
                device,
                name.to_lowercase()
            )?;

            let out = self.generate_device(&self.report.devices[*index], 16);
            write!(output, "{out}")?;

            writeln!(
                output,
                r##"
        }}"##
            )?;
        }

        for ((device, refdes), index) in &self.report.by_refdes {
            let name = refdes.to_lower_ident();
            write!(
                output,
                r##"
        #[allow(dead_code)]
        pub fn {device}_{name}(task: TaskId) -> I2cDevice {{"##,
            )?;

            let out = self.generate_device(&self.report.devices[*index], 16);
            write!(output, "{out}")?;

            writeln!(
                output,
                r##"
        }}"##
            )?;
        }

        writeln!(output, "    }}")?;

        if self.report.component_ids {
            let max_component_id_len = self.report.max_component_id_len;
            writeln!(
                output,
                r##"
        #[allow(dead_code)]
        pub const MAX_COMPONENT_ID_LEN: usize = {max_component_id_len};"##,
            )?;
        }

        self.generate_power(PowerDevices::PMBus, output)?;
        self.generate_power(PowerDevices::NonPMBus, output)?;

        Ok(())
    }

    pub fn generate_validation(&self, output: &mut String) -> Result<()> {
        let Some(validation) = &self.report.validation else {
            bail!(
                "internal error: validation code generation was requested, \
                 but validation was not analyzed"
            );
        };

        write!(
            output,
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
        // returned by `device_descriptions()`: if we change the ordering here,
        // it must be updated there as well.
        for (index, device) in self.report.devices.iter().enumerate() {
            match &validation[index] {
                Validation::Driver(driver) => {
                    let out = self.generate_device(device, 24);

                    write!(
                        output,
                        r##"
                {index} => {{
                    if drv_i2c_devices::{device}::{driver}::validate(&{out})? {{
                        Ok(I2cValidation::Good)
                    }} else {{
                        Ok(I2cValidation::Bad)
                    }}
                }}"##,
                        device = device.config.device,
                    )?;
                }
                Validation::RawRead => {
                    let out = self.generate_device(device, 20);
                    write!(
                        output,
                        r##"
                {index} => {{{out}.read::<u8>()?;
                    Ok(I2cValidation::RawReadOk)
                }}"##,
                    )?;
                }
            }
        }

        writeln!(
            output,
            r##"
                _ => Err(drv_i2c_api::ResponseCode::BadArg)
            }}
        }}
    }}"##
        )?;

        Ok(())
    }

    fn generate_power(
        &self,
        which: PowerDevices,
        output: &mut String,
    ) -> Result<()> {
        let rails = match which {
            PowerDevices::PMBus => &self.report.pmbus_rails,
            PowerDevices::NonPMBus => &self.report.power_rails,
        };

        if !rails.is_empty() {
            write!(
                output,
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

            for entry in rails {
                let rail = &entry.rail;
                let index = &entry.bank;
                let device = &self.report.devices[entry.device];

                // ---
                // Accessor, returns `(I2cDevice, Option<u8>)`

                write!(
                    output,
                    r##"
        #[allow(dead_code)]
        pub fn {}(task: TaskId)"##,
                    rail.to_lowercase(),
                )?;
                write!(output, " -> (I2cDevice, Option<u8>) {{")?;

                let out = self.generate_device(device, 16);
                if let Some(idx) = index {
                    writeln!(output, "({out}, Some({idx}))\n        }}")?;
                } else {
                    writeln!(output, "({out}, None)\n        }}")?;
                }

                if which == PowerDevices::PMBus {
                    let phases = match &entry.phases {
                        Some(phases) => {
                            let p = phases
                                .iter()
                                .map(|p| p.to_string())
                                .collect::<Vec<_>>()
                                .join(", ");

                            format!("Some(&[{p}])")
                        }
                        None => "None".to_string(),
                    };

                    writeln!(
                        output,
                        r##"
        #[allow(dead_code)]
        pub const {}_{rail}_PHASES: Option<&'static [u8]> = {phases};"##,
                        device.config.device.to_uppercase()
                    )?;
                }
            }

            writeln!(output, "    }}")?;
        }
        Ok(())
    }

    fn emit_sensor(
        &self,
        device: &str,
        label: &str,
        ids: &[usize],
        output: &mut String,
    ) -> Result<()> {
        let device = device.to_uppercase();
        let n_sensors = ids.len();
        writeln!(
            output,
            r##"
        #[allow(dead_code)]
        pub const NUM_{device}_{label}_SENSORS: usize = {n_sensors};"##,
        )?;

        if ids.len() == 1 {
            writeln!(
                output,
                r##"
        #[allow(dead_code)]
        pub const {device}_{label}_SENSOR: SensorId = SensorId::new({});"##,
                ids[0]
            )?;
        } else {
            writeln!(
                output,
                r##"
        #[allow(dead_code)]
        pub const {device}_{label}_SENSORS: [SensorId; {n_sensors}] = [ "##,
            )?;

            for id in ids {
                writeln!(output, "            SensorId::new({id}),",)?;
            }

            writeln!(output, "        ];")?;
        }

        Ok(())
    }

    fn declare_sensor_struct(
        &self,
        d: &I2cDevice,
        struct_name: &str,
        output: &mut String,
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
                output,
                "\n        #[allow(non_camel_case_types, dead_code)]
        pub struct Sensors_{struct_name} {{",
            )?;
            let mut f = |name, count| match count {
                0 => Ok(()),
                1 => writeln!(output, "            pub {name}: SensorId,"),
                _ => writeln!(
                    output,
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
            writeln!(output, "        }}")?;
        } else {
            writeln!(
                output,
                "\n        #[allow(dead_code, non_camel_case_types)]
        type Sensors_{struct_name} = ();",
            )?;
        }
        Ok(())
    }

    fn emit_sensor_struct(
        &self,
        d: &I2cDevice,
        label: String,
        name: &str,
        sensors: &[Arc<DeviceSensor>],
        output: &mut String,
    ) -> Result<()> {
        write!(
            output,
            "        #[allow(dead_code)]
        pub const {}_{label}_SENSORS: Sensors_{name} = ",
            d.device.to_uppercase(),
        )?;

        let mut sensors_by_kind: BTreeMap<Sensor, Vec<usize>> = BTreeMap::new();
        for s in sensors {
            sensors_by_kind.entry(s.kind).or_default().push(s.id);
        }
        if sensors_by_kind.is_empty() {
            writeln!(output, "();")?;
            return Ok(());
        }

        writeln!(output, "Sensors_{name} {{")?;

        for (kind, values) in sensors_by_kind {
            let field = match kind {
                Sensor::Temperature => "temperature",
                Sensor::Power => "power",
                Sensor::Current => "current",
                Sensor::Voltage => "voltage",
                Sensor::InputCurrent => "input_current",
                Sensor::InputVoltage => "input_voltage",
                Sensor::Speed => "speed",
                Sensor::Pwm => "pwm",
            };
            if values.len() == 1 {
                writeln!(
                    output,
                    "            {field}: SensorId::new({}),",
                    values[0]
                )?;
            } else {
                write!(output, "            {field}: [")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        write!(output, ", ")?;
                    }
                    write!(output, "SensorId::new({v})")?;
                }
                writeln!(output, "],")?;
            }
        }

        writeln!(output, "        }};")?;
        Ok(())
    }

    pub fn generate_sensors(&self, output: &mut String) -> Result<()> {
        let s = &self.report.sensors;

        write!(
            output,
            r##"
    pub mod sensors {{
        #[allow(unused_imports)]
        use super::super::SensorId;

        #[allow(dead_code)]
        pub const NUM_SENSORS: usize = {};
"##,
            s.total_sensors
        )?;

        for (i, d) in self.report.devices.iter().enumerate() {
            let info = &self.report.sensor_structs[i];

            if info.declare {
                self.declare_sensor_struct(&d.config, &info.name, output)?;
            }

            let sensors = s.device_sensors[i].as_slice();

            for label in &info.labels {
                self.emit_sensor_struct(
                    &d.config,
                    label.clone(),
                    &info.name,
                    sensors,
                    output,
                )?;
            }
        }

        for (k, ids) in s.by_device.iter() {
            self.emit_sensor(&k.device, &format!("{}", k.kind), ids, output)?;
        }

        for (k, ids) in s.by_name.iter() {
            let label = format!("{}_{}", k.name.to_uppercase(), k.kind);
            self.emit_sensor(&k.device, &label, ids, output)?;
        }

        for (k, ids) in s.by_refdes.iter() {
            let refdes = k.refdes.to_upper_ident();
            let label = format!("{refdes}_{}", k.kind);
            self.emit_sensor(&k.device, &label, ids, output)?;
        }

        writeln!(output, "\n    }}")?;
        Ok(())
    }

    pub fn generate_ports(&self, output: &mut String) -> Result<()> {
        writeln!(
            output,
            r##"
    pub mod ports {{"##
        )?;

        for ((controller, port), index) in &self.report.ports {
            writeln!(
                output,
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

        writeln!(output, "    }}")?;
        Ok(())
    }
}

fn match_arms<C>(
    out: &mut impl Write,
    source: &[(C, Vec<usize>)],
    fmt: impl Fn(&C) -> String,
) -> Result<()> {
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
            out,
            r##"
                {s} => {result},"##,
        )?;
    }
    Ok(())
}
