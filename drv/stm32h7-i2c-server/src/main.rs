//! A driver for the STM32H7 I2C interface

#![no_std]
#![no_main]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

use drv_i2c_api::*;
use drv_i2c_api::Port;
use drv_stm32h7_gpio_api::*;
use drv_stm32h7_i2c::*;
use drv_stm32h7_rcc_api::{Peripheral, Rcc};
use userlib::*;

mod ltc4306;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

#[cfg(feature = "standalone")]
const RCC: Task = SELF;

#[cfg(not(feature = "standalone"))]
const GPIO: Task = Task::gpio_driver;

#[cfg(feature = "standalone")]
const GPIO: Task = SELF;

cfg_if::cfg_if! {
    if #[cfg(target_board = "stm32h7b3i-dk")] {
        static mut I2C_CONTROLLERS: [I2cController; 1] = [ I2cController {
            controller: Controller::I2C4,
            peripheral: Peripheral::I2c4,
            getblock: device::I2C4::ptr,
            notification: (1 << (4 - 1)),
            registers: None,
            port: None,
        } ];

        const I2C_PINS: [I2cPin; 1] = [ I2cPin {
            controller: Controller::I2C4,
            port: Port::D,
            gpio_port: drv_stm32h7_gpio_api::Port::D,
            function: Alternate::AF4,
            mask: (1 << 12) | (1 << 13),
        } ];

        static mut I2C_MUXES: [I2cMux; 0] = [];
    } else if #[cfg(target_board = "nucleo-h743zi2")] {
        static mut I2C_CONTROLLERS: [I2cController; 1] = [ I2cController {
            controller: Controller::I2C2,
            peripheral: Peripheral::I2c2,
            getblock: device::I2C2::ptr,
            notification: (1 << (2 - 1)),
            registers: None,
            port: None,
        } ];

        const I2C_PINS: [I2cPin; 1] = [ I2cPin {
            controller: Controller::I2C2,
            port: Port::F,
            gpio_port: drv_stm32h7_gpio_api::Port::F,
            function: Alternate::AF4,
            mask: (1 << 0) | (1 << 1),
        } ];

        static mut I2C_MUXES: [I2cMux; 0] = [];
    } else if #[cfg(target_board = "gemini-bu-1")] {
        static mut I2C_CONTROLLERS: [I2cController; 3] = [ I2cController {
            controller: Controller::I2C1,
            peripheral: Peripheral::I2c1,
            getblock: device::I2C1::ptr,
            notification: (1 << (1 - 1)),
            registers: None,
            port: None,
        }, I2cController {
            controller: Controller::I2C3,
            peripheral: Peripheral::I2c3,
            getblock: device::I2C3::ptr,
            notification: (1 << (3 - 1)),
            registers: None,
            port: None,
        }, I2cController {
            controller: Controller::I2C4,
            peripheral: Peripheral::I2c4,
            getblock: device::I2C4::ptr,
            notification: (1 << (4 - 1)),
            registers: None,
            port: None,
        } ];

        const I2C_PINS: [I2cPin; 5] = [ I2cPin {
            controller: Controller::I2C1,
            port: Port::B,
            gpio_port: drv_stm32h7_gpio_api::Port::B,
            function: Alternate::AF4,
            mask: (1 << 8) | (1 << 9),
        }, I2cPin {
            controller: Controller::I2C4,
            port: Port::D,
            gpio_port: drv_stm32h7_gpio_api::Port::D,
            function: Alternate::AF4,
            mask: (1 << 12) | (1 << 13),
        }, I2cPin {
            controller: Controller::I2C4,
            port: Port::F,
            gpio_port: drv_stm32h7_gpio_api::Port::F,
            function: Alternate::AF4,
            mask: (1 << 14) | (1 << 15),
        }, I2cPin {
            controller: Controller::I2C3,
            port: Port::H,
            gpio_port: drv_stm32h7_gpio_api::Port::H,
            function: Alternate::AF4,
            mask: (1 << 7) | (1 << 8),
        }, I2cPin {
            controller: Controller::I2C4,
            port: Port::H,
            gpio_port: drv_stm32h7_gpio_api::Port::H,
            function: Alternate::AF4,
            mask: (1 << 11) | (1 << 12),
        } ];

        static mut I2C_MUXES: [I2cMux; 1] = [ I2cMux {
            controller: Controller::I2C4,
            port: Port::F,
            id: Mux::M1,
            driver: I2cMuxDriver::LTC4306,
            enable: (drv_stm32h7_gpio_api::Port::G, Alternate::AF0, (1 << 0)),
            address: 0x44,
            segment: None,
        } ];
    } else {
        compile_error!("no I2C controllers/pins for unknown board");
    }
}

fn lookup_controller(
    controller: Controller,
) -> Result<&'static mut I2cController<'static>, ResponseCode> {
    let controllers = unsafe { &mut I2C_CONTROLLERS };

    for c in controllers {
        if c.controller == controller {
            return Ok(c);
        }
    }

    Err(ResponseCode::BadController)
}

fn lookup_pin<'a>(
    controller: Controller,
    port: Port,
) -> Result<&'a I2cPin, ResponseCode> {
    let pins = &I2C_PINS;
    let mut default = None;

    for pin in pins {
        if pin.controller != controller {
            continue;
        }

        if pin.port == port {
            return Ok(pin);
        }

        if port == Port::Default {
            if default.is_none() {
                default = Some(pin);
            } else {
                return Err(ResponseCode::BadDefaultPort);
            }
        }
    }

    default.ok_or(ResponseCode::BadPort)
}

fn configure_mux(
    controller: &I2cController,
    port: Port,
    mux: Option<(Mux, Segment)>,
    enable: impl FnMut(u32) + Copy,
    wfi: impl FnMut(u32) + Copy,
) -> Result<(), ResponseCode> {
    match mux {
        Some((id, segment)) => {
            let muxes = unsafe { &mut I2C_MUXES };

            for mux in muxes {
                if mux.controller != controller.controller {
                    continue;
                }

                if mux.port != port || mux.id != id {
                    continue;
                }

                // We have our mux -- determine if the current segment matches
                // our specified segment...
                if let Some(current) = mux.segment {
                    if current == segment {
                        return Ok(());
                    }

                    // Beyond this point, we want any failure to set our new
                    // segment to leave our segment unset rather than having
                    // it point to the old segment.
                    mux.segment = None;
                } 

                // If we're here, our mux is valid, but the current segment is
                // not the specfied segment; we will now call upon our
                // driver to enable this segment.
                let enable_segment = match mux.driver {
                    I2cMuxDriver::LTC4306 => {
                        ltc4306::enable_segment
                    }
                };

                enable_segment(mux, controller, segment, enable, wfi)?;
                mux.segment = Some(segment);

                return Ok(());
            }

            Err(ResponseCode::MuxNotFound)
        }
        None => {
            Ok(())
        },
    }
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_i2c();
    configure_pins();
    configure_controllers();
    configure_muxes();

    // Field messages.
    let mut buffer = [0; 4];

    let enable = move |notification| {
        sys_irq_control(notification, true);
    };

    let wfi = move |notification| {
        let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
    };

    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::WriteRead => {
                let (payload, caller) = msg
                    .fixed_with_leases::<[u8; 4], ()>(2)
                    .ok_or(ResponseCode::BadArg)?;

                let (addr, controller, port, mux) =
                    Marshal::unmarshal(payload)?;

                if let Some(_) = ReservedAddress::from_u8(addr) {
                    return Err(ResponseCode::ReservedAddress);
                }

                let controller = lookup_controller(controller)?;
                let pin = lookup_pin(controller.controller, port)?;

                configure_port(controller, pin);
                configure_mux(controller, port, mux, enable, wfi)?;

                let wbuf = caller.borrow(0);
                let winfo = wbuf.info().ok_or(ResponseCode::BadArg)?;

                if !winfo.attributes.contains(LeaseAttributes::READ) {
                    return Err(ResponseCode::BadArg);
                }

                let rbuf = caller.borrow(1);
                let rinfo = rbuf.info().ok_or(ResponseCode::BadArg)?;

                if winfo.len == 0 && rinfo.len == 0 {
                    // We must have either a write OR a read -- while perhaps
                    // valid to support both being zero as a way of testing an
                    // address for a NACK, it's not a mode that we (currently)
                    // support.
                    return Err(ResponseCode::BadArg);
                }

                if winfo.len > 255 || rinfo.len > 255 {
                    // For now, we don't support writing or reading more than
                    // 255 bytes.
                    return Err(ResponseCode::BadArg);
                }

                match controller.write_read(
                    addr,
                    winfo.len,
                    |pos| wbuf.read_at(pos).unwrap(),
                    rinfo.len,
                    |pos, byte| rbuf.write_at(pos, byte).unwrap(),
                    enable,
                    wfi,
                ) {
                    Err(code) => Err(match code {
                        I2cError::NoDevice => ResponseCode::NoDevice,
                        I2cError::NoRegister => ResponseCode::NoRegister,
                    }),
                    Ok(_) => {
                        caller.reply(());
                        Ok(())
                    }
                }
            }
        });
    }
}

fn turn_on_i2c() {
    let controllers = unsafe { &I2C_CONTROLLERS };

    let rcc_driver = Rcc::from(TaskId::for_index_and_gen(
        RCC as usize,
        Generation::default(),
    ));

    for controller in controllers {
        controller.enable(&rcc_driver);
    }
}

fn configure_controllers() {
    let controllers = unsafe { &mut I2C_CONTROLLERS };

    for controller in controllers {
        controller.configure();
        sys_irq_control(controller.notification, true);
    }
}

fn configure_port(controller: &mut I2cController, pin: &I2cPin) {
    let p = controller.port.unwrap();

    if p == pin.port {
        return;
    }

    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);

    let src = lookup_pin(controller.controller, p).ok().unwrap();

    gpio_driver
        .configure(
            src.gpio_port,
            src.mask,
            Mode::Alternate,
            OutputType::OpenDrain,
            Speed::High,
            Pull::None,
            Alternate::AF0,
        )
        .unwrap();

    gpio_driver
        .configure(
            pin.gpio_port,
            pin.mask,
            Mode::Alternate,
            OutputType::OpenDrain,
            Speed::High,
            Pull::None,
            pin.function,
        )
        .unwrap();

    controller.port = Some(pin.port);
}

fn configure_pins() {
    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);

    for pin in &I2C_PINS {
        let controller = lookup_controller(pin.controller).ok().unwrap();

        match controller.port {
            None => {
                gpio_driver
                    .configure(
                        pin.gpio_port,
                        pin.mask,
                        Mode::Alternate,
                        OutputType::OpenDrain,
                        Speed::High,
                        Pull::None,
                        pin.function,
                    )
                    .unwrap();
                controller.port = Some(pin.port);
            }
            Some(_) => {}
        }
    }
}

fn configure_muxes() {
    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);
    let muxes = unsafe { &I2C_MUXES };

    for mux in muxes {
        gpio_driver
            .configure(
                mux.enable.0,
                mux.enable.2,
                Mode::Output,
                OutputType::PushPull,
                Speed::High,
                Pull::None,
                mux.enable.1,
            )
            .unwrap();

        gpio_driver
            .set_reset(mux.enable.0, mux.enable.2, 0)
            .unwrap();
    }
}
