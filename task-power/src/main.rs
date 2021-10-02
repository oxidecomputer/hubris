//! Power monitoring
//!
//! This is a primordial power monitoring task.
//!

#![no_std]
#![no_main]

use drv_i2c_api::*;
use drv_i2c_devices::adm1272::*;
use drv_i2c_devices::tps546b24a::*;
use ringbuf::*;
use userlib::units::*;
use userlib::*;

declare_task!(I2C, i2c_driver);

#[derive(Copy, Clone, PartialEq)]
enum Device {
    Adm1272,
    Tps546b24a,
}

#[derive(Copy, Clone, PartialEq)]
enum Command {
    VIn(Volts),
    VOut(Volts),
    IOut(Amperes),
    PeakIOut(Amperes),
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Datum(Device, Command),
    None,
}

ringbuf!(Trace, 16, Trace::None);

fn trace(dev: Device, cmd: Command) {
    ringbuf_entry!(Trace::Datum(dev, cmd));
}

#[export_name = "main"]
fn main() -> ! {
    let task = get_task_id(I2C);

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            const ADM1272_ADDRESS: u8 = 0x10;

            let mut adm1272 = Adm1272::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S3)),
                ADM1272_ADDRESS
            ), Ohms(0.001));

            const TPS546B24A_ADDRESS: u8 = 0x24;

            let mut tps546 = Tps546b24a::new(&I2cDevice::new(
                task,
                Controller::I2C4,
                Port::F,
                Some((Mux::M1, Segment::S4)),
                TPS546B24A_ADDRESS
            ));
        } else {
            cfg_if::cfg_if! {
                if #[cfg(feature = "standalone")] {
                    let device = I2cDevice::mock(task);
                    let mut adm1272 = Adm1272::new(&device, Ohms(0.0));
                    let mut tps546 = Tps546b24a::new(&device);
                } else {
                    compile_error!("unknown board");
                }
            }
        }
    }

    loop {
        match adm1272.read_vin() {
            Ok(volts) => {
                trace(Device::Adm1272, Command::VIn(volts));
            }
            Err(err) => {
                sys_log!("{}: VIn failed: {:?}", adm1272, err);
            }
        }

        match adm1272.read_vout() {
            Ok(volts) => {
                trace(Device::Adm1272, Command::VOut(volts));
            }
            Err(err) => {
                sys_log!("{}: VOut failed: {:?}", adm1272, err);
            }
        }

        match adm1272.read_iout() {
            Ok(amps) => {
                trace(Device::Adm1272, Command::IOut(amps));
            }
            Err(err) => {
                sys_log!("{}: IOut failed: {:?}", adm1272, err);
            }
        }

        match adm1272.peak_iout() {
            Ok(amps) => {
                trace(Device::Adm1272, Command::PeakIOut(amps));
            }
            Err(err) => {
                sys_log!("{}: PeakIOut failed: {:?}", adm1272, err);
            }
        }

        match tps546.read_vout() {
            Ok(volts) => {
                trace(Device::Tps546b24a, Command::VOut(volts));
            }

            Err(err) => {
                sys_log!("{}: VOut failed: {:?}", tps546, err);
            }
        }

        match tps546.read_iout() {
            Ok(amps) => {
                trace(Device::Tps546b24a, Command::IOut(amps));
            }

            Err(err) => {
                sys_log!("{}: IOut failed: {:?}", tps546, err);
            }
        }

        hl::sleep_for(1000);
    }
}
