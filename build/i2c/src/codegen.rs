// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Stage 3: code generation.

use crate::analysis::{DeviceSensor, I2cSensorsDescription};
use crate::load::{I2cDevice, I2cSensors, Sensor};
use crate::{CodegenOutputs, CodegenTarget, ConfigGenerator, Disposition};
use anyhow::{Result, bail};
use convert_case::{Case, Casing};
use multimap::MultiMap;
use rangemap::RangeSet;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::sync::Arc;

#[derive(PartialEq)]
enum PowerDevices {
    /// PMBus power devices
    PMBus,

    /// Non-PMBus power devices
    NonPMBus,
}

impl ConfigGenerator {
    pub fn ncontrollers(&self) -> usize {
        self.controllers.len()
    }

    pub fn generate_header(&self, output: &mut String) -> Result<()> {
        writeln!(output, "pub(crate) mod i2c_config {{")?;
        Ok(())
    }

    pub fn generate_footer(&self, output: &mut String) -> Result<()> {
        writeln!(output, "}}")?;
        Ok(())
    }

    pub fn generate_controllers(&self, output: &mut String) -> Result<()> {
        match self.settings.disposition {
            Disposition::Initiator | Disposition::Target => {}

            _ => {
                panic!("illegal disposition for controller generation");
            }
        }

        writeln!(
            output,
            r##"
    #[allow(dead_code)]
    pub const NCONTROLLERS: usize = {ncontrollers};

    use drv_stm32xx_i2c::I2cController;

    pub fn controllers() -> [I2cController<'static>; NCONTROLLERS] {{"##,
            ncontrollers = self.controllers.len()
        )?;

        if !self.controllers.is_empty() {
            writeln!(
                output,
                r##"
        use drv_stm32xx_sys_api::Peripheral;
        use drv_i2c_api::Controller;"##
            )?;

            let text = match self.settings.codegen_target {
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

        for c in &self.controllers {
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

        match self.settings.disposition {
            Disposition::Initiator | Disposition::Target => {}

            _ => {
                panic!("illegal disposition for pin generation");
            }
        }

        for c in &self.controllers {
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

        for c in &self.controllers {
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
        if self.settings.disposition == Disposition::Target {
            panic!("cannot generate muxes when configured as target");
        }

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

        let component_id = if self.settings.component_ids {
            if let Some(ref refdes) = d.refdes {
                let id = refdes.to_component_id();
                format!("\n{indent}    {id:?},")
            } else {
                println!(
                    "cargo::error=device {} has no refdes, but we were asked to generate component IDs",
                    d.device
                );
                String::new()
            }
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
{indent}    {address:#x},{component_id}
{indent})"##,
            description = d.description,
            controller = controller,
            port = port,
            segment = segment,
            address = d.address,
            indent = indent,
        )
    }

    pub fn generate_devices(&self, output: &mut String) -> Result<()> {
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

            if let Some(name) = &d.name
                && by_name.insert((&d.device, name), d).is_some()
            {
                panic!("duplicate name {} for device {}", name, d.device)
            }
            if let Some(refdes) = &d.refdes {
                if by_refdes.insert((&d.device, refdes), d).is_some() {
                    panic!(
                        "duplicate refdes {refdes:?} for device {}",
                        d.device
                    )
                } else if by_name
                    .contains_key(&(&d.device, &refdes.to_upper_ident()))
                {
                    panic!(
                        "refdes {refdes:?} for device {} is also a device name",
                        d.device
                    )
                }
            }
        }

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
        let task_arg = if self.devices.is_empty() {
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

        for (index, device) in self.devices.iter().enumerate() {
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

        let mut all: Vec<_> = by_controller.iter_all().collect();
        all.sort();

        match_arms(output, all, |c| format!("Some(Controller::I2C{c})"))?;

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

        let mut all: Vec<_> = by_port.iter_all().collect();
        all.sort();

        match_arms(output, all, |p| format!("Some(PortIndex({p}))"))?;

        write!(
            output,
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
                output,
                r##"
        #[allow(dead_code)]
        pub fn {}(task: TaskId) -> [I2cDevice; {}] {{
            ["##,
                device,
                devices.len()
            )?;

            for d in devices {
                let out = self.generate_device(d, 16);
                write!(output, "{out},")?;
            }

            writeln!(
                output,
                r##"
            ]
        }}"##
            )?;
        }

        let mut all: Vec<_> = by_bus.iter_all().collect();
        all.sort();

        for ((device, bus), devices) in all {
            write!(
                output,
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
                write!(output, "{out},")?;
            }
            writeln!(
                output,
                r##"
            ]
        }}"##
            )?;
        }

        let mut all: Vec<_> = by_name.iter().collect();
        all.sort();
        for ((device, name), d) in &all {
            write!(
                output,
                r##"
        #[allow(dead_code)]
        pub fn {}_{}(task: TaskId) -> I2cDevice {{"##,
                device,
                name.to_lowercase()
            )?;

            let out = self.generate_device(d, 16);
            write!(output, "{out}")?;

            writeln!(
                output,
                r##"
        }}"##
            )?;
        }

        let mut all: Vec<_> = by_refdes.iter().collect();
        all.sort();

        let mut max_component_id_len = 0;
        for ((device, refdes), d) in &all {
            max_component_id_len = max_component_id_len.max(refdes.len());
            let name = refdes.to_lower_ident();
            write!(
                output,
                r##"
        #[allow(dead_code)]
        pub fn {device}_{name}(task: TaskId) -> I2cDevice {{"##,
            )?;

            let out = self.generate_device(d, 16);
            write!(output, "{out}")?;

            writeln!(
                output,
                r##"
        }}"##
            )?;
        }

        writeln!(output, "    }}")?;

        if self.settings.component_ids {
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
        let drivers = &self.settings.drivers;

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
        // returned by `device_descriptions()` below: if we change the ordering
        // here, it must be updated there as well.
        for (index, device) in self.devices.iter().enumerate() {
            if drivers.contains(&device.device) {
                if device.validate_with_raw_read {
                    bail!(
                        "Device '{}{}{}' set `validate-with-raw-read = true`, \
                        but that was probably a mistake because this device \
                        already has a driver in `drv-i2c-devices` that should \
                        be able to perform better, device-specific validation.",
                        device.device,
                        device
                            .name
                            .as_ref()
                            .map(|name| format!(" {}", name))
                            .unwrap_or_default(),
                        device
                            .refdes
                            .as_ref()
                            .map(|refdes| format!(" {:?}", refdes))
                            .unwrap_or_default()
                    );
                }

                let driver = device.device.to_case(Case::UpperCamel);
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
                    device = device.device,
                )?;
            } else {
                if !device.validate_with_raw_read {
                    bail!(
                        "Device '{}{}{}' has no driver in `drv-i2c-devices`. \
                        You must either add a driver that implements the \
                        `Validate` trait or set `validate-with-raw-read = \
                        true` to opt in to a generic implementation instead.",
                        device.device,
                        device
                            .name
                            .as_ref()
                            .map(|name| format!(" {}", name))
                            .unwrap_or_default(),
                        device
                            .refdes
                            .as_ref()
                            .map(|refdes| format!(" {:?}", refdes))
                            .unwrap_or_default()
                    );
                }
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
                    let single = rails.len() == 1;
                    for (index, rail) in rails.iter().enumerate() {
                        if rail.is_empty() {
                            continue;
                        }

                        let idx = if single { None } else { Some(index) };

                        if byrail.insert(rail, (d, idx)).is_some() {
                            bail!("duplicate rail {rail}");
                        }
                    }
                }
            }
        }

        if !byrail.is_empty() {
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

            let mut all: Vec<_> = byrail.iter().collect();
            all.sort();

            for (rail, (device, index)) in &all {
                let raw_bank = index.unwrap_or(0);

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
                    let phases = if let Some(power) = &device.power {
                        if let Some(phases) = &power.phases {
                            let p = phases[raw_bank]
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
                        output,
                        r##"
        #[allow(dead_code)]
        pub const {}_{rail}_PHASES: Option<&'static [u8]> = {phases};"##,
                        device.device.to_uppercase()
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

    pub(crate) fn sensors_description(&self) -> I2cSensorsDescription {
        I2cSensorsDescription::new(&self.devices)
    }

    pub fn generate_sensors(
        &self,
        output: &mut String,
    ) -> Result<I2cSensorsDescription> {
        let s = self.sensors_description();

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
                self.declare_sensor_struct(d, &struct_name, output)?;
            }
            let s = s.device_sensors[i].as_slice();
            if let Some(name) = &d.name {
                self.emit_sensor_struct(
                    d,
                    name.to_uppercase(),
                    &struct_name,
                    s,
                    output,
                )?;
            }
            if let Some(refdes) = &d.refdes {
                self.emit_sensor_struct(
                    d,
                    refdes.to_upper_ident(),
                    &struct_name,
                    s,
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
        Ok(s)
    }

    pub fn generate_ports(&self, output: &mut String) -> Result<()> {
        writeln!(
            output,
            r##"
    pub mod ports {{"##
        )?;

        for ((controller, port), index) in &self.ports {
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

    /// Use the given ConfigGenerator to do code generation.
    ///
    /// This does not write the output to a file, but returns the generated code
    /// in the `code` field of the `CodegenOutputs` struct. Using
    /// [`codegen_to_file`] will also write the generated code to the
    /// `i2c_config.rs` file for the task currently being built. This method may
    /// be used instead when running codegen outside of a task.
    pub fn codegen(self) -> Result<CodegenOutputs> {
        let mut output = String::new();
        self.generate_header(&mut output)?;

        let mut output_sensors = None;
        match self.settings.disposition {
            Disposition::Target => {
                let n = self.ncontrollers();

                if n != 1 {
                    //
                    // If we have the disposition of a target, we expect exactly
                    // one controller to be configured as a target; if none have
                    // been specified, the task should be deconfigured.
                    //
                    anyhow::bail!(
                        "found {n} I2C controller(s); expected exactly one"
                    );
                }

                self.generate_controllers(&mut output)?;
                self.generate_pins(&mut output)?;
                self.generate_ports(&mut output)?;
            }

            Disposition::Initiator => {
                self.generate_controllers(&mut output)?;
                self.generate_pins(&mut output)?;
                self.generate_ports(&mut output)?;
                self.generate_muxes(&mut output)?;
            }

            Disposition::Devices => {
                self.generate_devices(&mut output)?;
                self.generate_ports(&mut output)?;
            }

            Disposition::Sensors => {
                self.generate_devices(&mut output)?;
                let desc = self.generate_sensors(&mut output)?;
                output_sensors = Some(desc);
            }

            Disposition::Validation => {
                self.generate_devices(&mut output)?;
                self.generate_validation(&mut output)?;
            }
        }

        self.generate_footer(&mut output)?;

        Ok(CodegenOutputs {
            code: output,
            sensors: output_sensors,
        })
    }
}

fn match_arms<'a, C>(
    out: &mut impl Write,
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
            out,
            r##"
                {s} => {result},"##,
        )?;
    }
    Ok(())
}
