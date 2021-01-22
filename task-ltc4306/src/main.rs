#![no_std]
#![no_main]

// Make sure we actually link in userlib, despite not using any of it explicitly
// - we need it for our _start routine.
use userlib::*;
use ringbuf::*;
use drv_i2c_api::*;

#[cfg(feature = "standalone")]
const I2C: Task = SELF;

#[cfg(not(feature = "standalone"))]
const I2C: Task = Task::i2c_driver;

ringbuf!(bool, 32, false);

cfg_if::cfg_if! {
    if #[cfg(feature = "stm32h7")] {
        cfg_if::cfg_if! {
            if #[cfg(feature = "standalone")] {
                const GPIO: Task = Task::anonymous;
            } else {
                const GPIO: Task = Task::gpio_driver;
            }
        }

        cfg_if::cfg_if! {
            if #[cfg(target_board = "gemini-bu-1")] {
                const LTC4306_PORT: drv_stm32h7_gpio_api::Port =
                    drv_stm32h7_gpio_api::Port::G;
                const LTC4306_MASK: u16 = 1 << 0;
            } else {
                compile_error!("no known LTC4306 for this board");
            }
        }
    }
}

#[cfg(feature = "stm32h7")]
fn configure_ltc4306() {
    use drv_stm32h7_gpio_api::*;

    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);

    gpio_driver
        .configure(
            LTC4306_PORT,
            LTC4306_MASK,
            Mode::Output,
            OutputType::PushPull,
            Speed::High,
            Pull::None,
            Alternate::AF0,
        )
        .unwrap();

    gpio_driver.set_reset(LTC4306_PORT, LTC4306_MASK, 0).unwrap();
}

#[export_name = "main"]
fn main() -> ! {
    #[cfg(feature = "stm32h7")]
    configure_ltc4306();

    loop {
        ringbuf_entry!(true);
        hl::sleep_for(1000);
        ringbuf_entry!(false);
        hl::sleep_for(1000);
    }
}
