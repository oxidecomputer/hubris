//! Power monitoring
//!
//! This is a primordial power monitoring task.
//!

#![no_std]
#![no_main]

use drv_i2c_devices::adm1272::*;
use drv_i2c_devices::tps546b24a::*;
use drv_i2c_devices::isl68224::*;
use ringbuf::*;
use userlib::units::*;
use userlib::*;

task_slot!(I2C, i2c_driver);
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[derive(Copy, Clone, PartialEq)]
enum Device {
    Adm1272,
    Tps546b24a,
    Isl68224,
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
    let task = I2C.get_task_id();
    use i2c_config::devices;

    cfg_if::cfg_if! {
        if #[cfg(target_board = "gemini-bu-1")] {
            let mut adm1272 = Adm1272::new(
                &devices::adm1272(task)[0],
                Ohms(0.001)
            );

            let mut tps546 = Tps546b24a::new(&devices::tps546b24a(task)[0]);

            let (device, rail) = i2c_config::pmbus::isl_evl_vout0(task);
            let mut isl = Isl68224::new(&device);
            isl.set_rail(rail);

        } else {
            cfg_if::cfg_if! {
                if #[cfg(feature = "standalone")] {
                    let device = &devices::mock(task);
                    let mut adm1272 = Adm1272::new(&device, Ohms(0.0));
                    let mut tps546 = Tps546b24a::new(&device);
                } else {
                    compile_error!("unknown board");
                }
            }
        }
    }

    loop {
        match isl.read_vout() {
            Ok(volts) => {
                trace(Device::I, Command::VIn(volts));

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
