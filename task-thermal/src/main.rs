//! Thermal loop
//!
//! This is a primordial thermal loop, which will ultimately reading temperature
//! sensors and control fan duty cycles to actively manage thermals.  Right now,
//! though it is merely reading every fan and temp sensor that it can find...
//!

#![no_std]
#![no_main]

use drv_i2c_api::*;
use drv_i2c_devices::adt7420::*;
use drv_i2c_devices::ds2482::*;
use drv_i2c_devices::max31790::*;
use drv_i2c_devices::max6634::*;
use drv_i2c_devices::mcp9808::*;
use drv_i2c_devices::pct2075::*;
use drv_i2c_devices::tmp116::*;
use drv_i2c_devices::TempSensor;
use drv_onewire_devices::ds18b20::*;
use userlib::units::*;
use userlib::*;

declare_task!(I2C, i2c_driver);

fn convert_fahrenheit(temp: Celsius) -> f32 {
    temp.0 * (9.0 / 5.0) + 32.0
}

fn print_temp<T: core::fmt::Display>(temp: Celsius, device: &T) {
    let f = convert_fahrenheit(temp);

    sys_log!(
        "{}: temp is {}.{:03} degrees C, {}.{:03} degrees F",
        device,
        temp.0 as i32,
        (((temp.0 + 0.0005) * 1000.0) as i32) % 1000,
        f as i32,
        (((f + 0.0005) * 1000.0) as i32) % 1000
    );
}

fn read_fans(fctrl: &Max31790) {
    let mut ndx = 0;

    for fan in 0..MAX_FANS {
        let fan = Fan::new(fan).unwrap();

        match fctrl.fan_rpm(fan) {
            Ok(rval) if rval.0 != 0 => {
                sys_log!("{}: {}: RPM={}", fctrl, fan, rval.0);
            }
            Ok(_) => {}
            Err(err) => {
                sys_log!("{}: {}: failed: {:?}", fctrl, fan, err);
            }
        }

        ndx = ndx + 1;
    }
}

fn ds2482_search(ds2482: &mut Ds2482, devices: &mut [Option<Ds18b20>]) {
    if let Err(err) = ds2482.initialize() {
        sys_log!("{}: failed to initialize: {:?}", ds2482, err);
        return;
    }

    let mut ndevices = 0;

    loop {
        match ds2482.search() {
            Ok(Some(id)) => {
                if ndevices == devices.len() {
                    sys_log!("{}: too many 1-wire devices found");
                    return;
                }

                if let Some(dev) = Ds18b20::new(id) {
                    sys_log!("{}: found {}", ds2482, dev);
                    devices[ndevices] = Some(dev);
                    ndevices += 1;
                } else {
                    sys_log!("{}: non-DS18B20 found: 0x{:016x}", ds2482, id);
                }
            }

            Ok(None) => {
                break;
            }
            Err(err) => {
                sys_log!("{}: failed search: {:?}", ds2482, err);
                return;
            }
        }
    }
}

fn ds18b20_read(device: &Ds18b20, ds2482: &Ds2482) {
    let reset = || ds2482.reset();
    let write = |byte| ds2482.write_byte(byte);
    let read = || ds2482.read_byte();

    match device.read_temperature(reset, write, read) {
        Ok(temp) => {
            print_temp(temp, device);
        }

        Err(_) => {
            sys_log!("failed to read temp!");
        }
    }
}

fn ds18b20_kick_off_conversion(device: &Ds18b20, ds2482: &Ds2482) {
    let reset = || ds2482.reset();
    let write = |byte| ds2482.write_byte(byte);

    if let Err(err) = device.convert_temperature(reset, write) {
        sys_log!("{}: conversion failed: {:?}", device, err);
    }
}

fn temp_read<E: core::fmt::Debug, T: TempSensor<E> + core::fmt::Display>(
    device: &T,
) {
    match device.read_temperature() {
        Ok(temp) => {
            print_temp(temp, device);
        }

        Err(err) => {
            sys_log!("{}: failed to read temp: {:?}", device, err);
        }
    }
}

fn adt7420_read(device: &Adt7420, validated: &mut bool) {
    if *validated {
        temp_read(device);
    } else {
        match device.validate() {
            Ok(_) => {
                sys_log!("{}: found device!", device);
                *validated = true;
            }
            Err(err) => {
                sys_log!("{}: no bueno: {:?}", device, err);
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let task = get_task_id(I2C);

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            const MAX31790_ADDRESS: u8 = 0x20;

            let fctrl = Max31790::new(&I2cDevice::new(
                task,
                Controller::I2C1,
                Port::Default,
                None,
                MAX31790_ADDRESS,
            ));

            const ADT7420_ADDRESS: u8 = 0x48;

            let mut adt7420 = [ (Adt7420::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S1)),
                ADT7420_ADDRESS
            )), false), (Adt7420::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S2)),
                ADT7420_ADDRESS
            )), false), (Adt7420::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S4)),
                ADT7420_ADDRESS
            )), false)];

            const MAX6634_ADDRESS: u8 = 0x48;

            let max6634 = [ Max6634::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                Some((Mux::M1, Segment::S1)),
                MAX6634_ADDRESS
            )), Max6634::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                Some((Mux::M1, Segment::S2)),
                MAX6634_ADDRESS + 1
            )), Max6634::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                Some((Mux::M1, Segment::S3)),
                MAX6634_ADDRESS + 2
            )), Max6634::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                Some((Mux::M1, Segment::S4)),
                MAX6634_ADDRESS + 3
            )), Max6634::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                Some((Mux::M1, Segment::S5)),
                MAX6634_ADDRESS + 4
            )), Max6634::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                Some((Mux::M1, Segment::S6)),
                MAX6634_ADDRESS + 5
            )), Max6634::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                Some((Mux::M1, Segment::S7)),
                MAX6634_ADDRESS + 6
            )), Max6634::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                Some((Mux::M1, Segment::S8)),
                MAX6634_ADDRESS + 7
            )) ];

            const TMP116_ADDRESS: u8 = 0x48;

            let tmp116 = [ Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::H,
                None,
                TMP116_ADDRESS
            )), Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::H,
                None,
                TMP116_ADDRESS + 1
            )) ];

            const MCP9808_ADDRESS: u8 = 0x18;

            let mcp9808 = [ Mcp9808::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                None,
                MCP9808_ADDRESS
            )) ];

            const PCT2075_ADDRESS: u8 = 0x37;

            let pct2075 = [ Pct2075::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::D,
                None,
                PCT2075_ADDRESS
            )) ];

            const DS2482_ADDRESS: u8 = 0x19;

            let mut ds2482 = Ds2482::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S3)),
                DS2482_ADDRESS,
            ));
        } else if #[cfg(target_board = "gimlet-1")] {
            // Two sets of TMP117 sensors, Front and Rear
            // These all have the same address but are on different
            // controllers/ports

            const TMP116_ADDRESS: u8 = 0x48;

            // Front sensors (U.2)
            let tmp116 = [ Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C2,
                Port::F,
                None,
                TMP116_ADDRESS
            )), Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C2,
                Port::F,
                None,
                TMP116_ADDRESS + 1
            )),
                Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C2,
                Port::F,
                None,
                TMP116_ADDRESS + 2
            )),

            // Rear sensors (fans)
                Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                None,
                TMP116_ADDRESS
            )), Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                None,
                TMP116_ADDRESS + 1
            )),
                Tmp116::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                None,
                TMP116_ADDRESS + 2
            )),
            ];


            const MAX31790_ADDRESS: u8 = 0x20;

            let fctrl = Max31790::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                None,
                MAX31790_ADDRESS,
            ));

            // XXX Something better?
            let device = I2cDevice::new(task, Controller::I2C4, Port::F, None, 0xff);

            let mut ds2482 = Ds2482::new(&device);
            let mut adt7420 = [ (Adt7420::new(&device), false) ];
            let max6634 = [ Max6634::new(&device) ];
            let mcp9808 = [ Mcp9808::new(&device) ];
            let pct2075 = [ Pct2075::new(&device) ];
        } else {
            cfg_if::cfg_if! {
                if #[cfg(feature = "standalone")] {
                    let device = I2cDevice::mock(task);
                    let fctrl = Max31790::new(&device);
                    let mut adt7420 = [ (Adt7420::new(&device), false) ];
                    let max6634 = [ Max6634::new(&device) ];
                    let tmp116 = [ Tmp116::new(&device) ];
                    let mcp9808 = [ Mcp9808::new(&device) ];
                    let pct2075 = [ Pct2075::new(&device) ];
                    let mut ds2482 = Ds2482::new(&device);
                } else {
                    compile_error!("unknown board");
                }
            }
        }
    }

    loop {
        match fctrl.initialize() {
            Ok(_) => {
                sys_log!("{}: initialization successful", fctrl);
                break;
            }
            Err(err) => {
                sys_log!("{}: initialization failed: {:?}", fctrl, err);
                hl::sleep_for(1000);
            }
        }
    }

    let mut ds18b20: [Option<Ds18b20>; 8] = [None; 8];
    ds2482_search(&mut ds2482, &mut ds18b20);

    loop {
        read_fans(&fctrl);

        for (device, validated) in &mut adt7420 {
            adt7420_read(device, validated);
        }

        for device in &ds18b20 {
            if let Some(device) = device {
                ds18b20_read(device, &ds2482);
            }
        }

        for device in &max6634 {
            temp_read(device);
        }

        for device in &tmp116 {
            temp_read(device);
        }

        for device in &mcp9808 {
            temp_read(device);
        }

        for device in &pct2075 {
            temp_read(device);
        }

        // Before we go to sleep, kick off (asynchronous) temp conversions
        // for our DS18B20s -- these can take up to 750ms!
        for device in &ds18b20 {
            if let Some(device) = device {
                ds18b20_kick_off_conversion(device, &ds2482);
            }
        }

        hl::sleep_for(1000);
    }
}
