#![no_std]
#![no_main]

use drv_i2c_api::*;
use userlib::*;

mod ds2482;
use ds2482::*;

mod ds18b20;
use ds18b20::*;

mod onewire;

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

fn convert_fahrenheit(temp: f32) -> f32 {
    temp * (9.0 / 5.0) + 32.0
}

#[export_name = "main"]
fn main() -> ! {
    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            const DS2482_ADDRESS: u8 = 0x19;

            let i2c = I2c::new(
                TaskId::for_index_and_gen(I2C as usize, Generation::default()),
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S3)),
                DS2482_ADDRESS,
            );
        } else {
            compile_error!("unknown board");
        }
    }

    let mut ds2482 = Ds2482::new(&i2c);

    if let Err(_) = ds2482.initialize() {
        panic!("failed to initialize!");
    }

    let mut devices: [Option<Ds18b20>; 32] = [None; 32];
    let mut ndevices = 0;

    loop {
        match ds2482.search() {
            Ok(Some(id)) => {
                if ndevices == devices.len() {
                    panic!("too many 1-wire devices found");
                }

                if let Some(dev) = Ds18b20::new(id) {
                    devices[ndevices] = Some(dev);
                    ndevices += 1;
                } else {
                    sys_log!("non-DS18B20 found: 0x{:016x}", id);
                }
            }

            Ok(None) => {
                break;
            }
            Err(_) => {
                panic!("search failed!");
            }
        }
    }

    loop {
        for i in 0..ndevices {
            let dev = devices[i].unwrap();

            if let Err(_) = dev
                .convert_temp(|| ds2482.reset(), |byte| ds2482.write_byte(byte))
            {
                sys_log!("ds2482: failed to convert {:x}", dev.id);
            }
        }

        hl::sleep_for(1000);

        for i in 0..ndevices {
            let dev = devices[i].unwrap();

            match dev.read_temp(
                || ds2482.reset(),
                |byte| ds2482.write_byte(byte),
                || ds2482.read_byte(),
            ) {
                Ok(temp) => {
                    let f = convert_fahrenheit(temp);

                    sys_log!(
                        "ds2482: {:x}: temp is {}.{:03} degrees C, \
                        {}.{:03} degrees F",
                        dev.id,
                        temp as i32,
                        (((temp + 0.0005) * 1000.0) as i32) % 1000,
                        f as i32,
                        (((f + 0.0005) * 1000.0) as i32) % 1000
                    );
                }
                Err(_) => {
                    sys_log!("failed to read temp!");
                }
            }
        }
    }
}
