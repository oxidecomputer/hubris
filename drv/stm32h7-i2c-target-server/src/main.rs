#![no_std]
#![no_main]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

use userlib::*;
use drv_i2c_api::{Controller, Port};
use drv_stm32h7_rcc_api::{Peripheral, Rcc};
use drv_stm32h7_gpio_api::*;
use drv_stm32h7_i2c::*;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

#[cfg(feature = "standalone")]
const RCC: Task = SELF;

#[cfg(not(feature = "standalone"))]
const GPIO: Task = Task::gpio_driver;

#[cfg(feature = "standalone")]
const GPIO: Task = SELF;

cfg_if::cfg_if! {
    if #[cfg(target_board = "gemini-bu-1")] {
        static mut I2C_CONTROLLER: I2cController = I2cController {
            controller: Controller::I2C2,
            peripheral: Peripheral::I2c2,
            getblock: device::I2C2::ptr,
            notification: (1 << (2 - 1)),
            registers: None,
            port: None,
        };

        const I2C_PIN: I2cPin = I2cPin {
            controller: Controller::I2C2,
            port: Port::F,
            gpio_port: drv_stm32h7_gpio_api::Port::F,
            function: Alternate::AF4,
            mask: (1 << 0) | (1 << 1),
        };
    } else {
        compile_error!("I2C target unsupported for this board");
    }
}

fn configure_pin() {
    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);

    let pin = &I2C_PIN;
    let controller = unsafe { &mut I2C_CONTROLLER };

    gpio_driver
        .configure(
            pin.gpio_port,
            pin.mask,
            Mode::Alternate,
            OutputType::OpenDrain,
            Speed::High,
            Pull::None,
            pin.function
        )
        .unwrap();

    controller.port = Some(pin.port);
}

#[export_name = "main"]
fn main() -> ! {
    let controller = unsafe { &mut I2C_CONTROLLER };

    // Enable the controller
    let rcc_driver = Rcc::from(TaskId::for_index_and_gen(
        RCC as usize,
        Generation::default(),
    ));

    controller.enable(&rcc_driver);

    // Configure our pins
    configure_pin();

    let wfi = |notification| {
        let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
    };

    let mut response = |register, buf: &mut [u8]| -> Option<usize> {
        match register {
            Some(val) => { 
                buf[0] = val;
                Some(1)
            }
            _ => { Some(0) }
        }
    };

    let enable = |notification| {
        sys_irq_control(notification, true);
    };

    controller.operate_as_target(0x1d, enable, wfi, &mut response);
}
