//! A driver for the STM32H7 I2C interface

#![no_std]
#![no_main]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h7b3")]
use device::i2c3::RegisterBlock;

#[cfg(feature = "h743")]
use device::i2c1::RegisterBlock;

use userlib::*;
use drv_i2c_api::{Controller, Op, ReservedAddress, Port, ResponseCode};
use drv_stm32h7_rcc_api::{Peripheral, Rcc};
use drv_stm32h7_gpio_api::*;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

#[cfg(feature = "standalone")]
const RCC: Task = SELF;

#[cfg(not(feature = "standalone"))]
const GPIO: Task = Task::gpio_driver;

#[cfg(feature = "standalone")]
const GPIO: Task = SELF;

struct I2cPin {
    controller: Controller,
    port: Port,
    gpio_port: drv_stm32h7_gpio_api::Port,
    function: Alternate,
    mask: u16,
}

struct I2cController<'a> {
    controller: Controller,
    peripheral: Peripheral,
    getblock: fn() -> *const RegisterBlock,
    notification: u32,
    port: Option<Port>,
    registers: Option<&'a RegisterBlock>,
}

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
    } else if #[cfg(target_board = "gemini-bu-1")] {
        static mut I2C_CONTROLLERS: [I2cController; 4] = [ I2cController {
            controller: Controller::I2C1,
            peripheral: Peripheral::I2c1,
            getblock: device::I2C1::ptr,
            notification: (1 << (1 - 1)),
            registers: None,
            port: None,
        }, I2cController {
            controller: Controller::I2C2,
            peripheral: Peripheral::I2c2,
            getblock: device::I2C2::ptr,
            notification: (1 << (2 - 1)),
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

        const I2C_PINS: [I2cPin; 6] = [ I2cPin {
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
            controller: Controller::I2C2,
            port: Port::F,
            gpio_port: drv_stm32h7_gpio_api::Port::F,
            function: Alternate::AF4,
            mask: (1 << 0) | (1 << 1),
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
    } else {
        compile_error!("no I2C controllers/pins for unknown board");
    }
}

fn lookup_controller(
    controller: u8
) -> Result<&'static mut I2cController<'static>, ResponseCode> {

    match Controller::from_u8(controller) {
        Some(controller) => {
            let controllers = unsafe { &mut I2C_CONTROLLERS };

            for mut c in controllers {
                if c.controller == controller {
                    return Ok(c);
                }
            }
        }
        _ => {}
    }

    Err(ResponseCode::BadController)
}

fn lookup_pin<'a>(
    controller: Controller,
    port: Port
) -> Result<&'a I2cPin, ResponseCode> {
    let pins = unsafe { &I2C_PINS };
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

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_i2c();
    configure_pins();
    configure_controllers();

    // Field messages.
    let mut buffer = [0; 3];

    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::WriteRead => {
                let (&[addr, controller, port], caller) = msg
                    .fixed_with_leases::<[u8; 3], ()>(2)
                    .ok_or(ResponseCode::BadArg)?;

                let port = Port::from_u8(port).ok_or(ResponseCode::BadPort)?;
                let controller = lookup_controller(controller)?;
                let pin = lookup_pin(controller.controller, port)?;

                let wbuf = caller.borrow(0);
                let winfo = wbuf.info().ok_or(ResponseCode::BadArg)?;

                if !winfo.attributes.contains(LeaseAttributes::READ) {
                    return Err(ResponseCode::BadArg);
                }

                let rbuf = caller.borrow(1);
                let rinfo = rbuf.info().ok_or(ResponseCode::BadArg)?;

                if let Some(_) = ReservedAddress::from_u8(addr) {
                    return Err(ResponseCode::ReservedAddress);
                }

                configure_port(controller, pin);

                write_read(
                    controller.registers.unwrap(),
                    controller.notification,
                    addr,
                    winfo.len,
                    |pos| { wbuf.read_at(pos) },
                    rinfo.len,
                    |pos, byte| { rbuf.write_at(pos, byte) },
                )?;

                caller.reply(());
                Ok(())
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
        rcc_driver.enable_clock(controller.peripheral);
        rcc_driver.leave_reset(controller.peripheral);
    }
}

fn configure_controller(i2c: &RegisterBlock) {
    // Disable PE
    i2c.cr1.write(|w| { w.pe().clear_bit() });

    cfg_if::cfg_if! {
        if #[cfg(feature = "h7b3")] {
            // We want to set our timing to achieve a 100kHz SCL. Given our
            // APB4 peripheral clock of 280MHz, here is how we configure our
            // timing:
            //
            // - A PRESC of 7, yielding a t_presc of 28.57 ns.
            // - An SCLH of 137 (0x89), yielding a t_sclh of 3942.86 ns.
            // - An SCLL of 207 (0xcf), yielding a t_scll of 5942.86 ns.
            //
            // Taken together, this yields a t_scl of 9885.71 ns.  Which, when
            // added to our t_sync1 and t_sync2 will be close to our target of
            // 10000 ns.  Finally, we set SCLDEL to 8 and SDADEL to 0 --
            // values that come from the STM32CubeMX tool (as advised by
            // 52.4.10).
            i2c.timingr.write(|w| { w
                .presc().bits(7)
                .sclh().bits(137)
                .scll().bits(207)
                .scldel().bits(8)
                .sdadel().bits(0)
            });
        } else if #[cfg(feature = "h743")] {
            // Here our APB1 peripheral clock is 100MHz, yielding the
            // following:
            //
            // - A PRESC of 1, yielding a t_presc of 20 ns
            // - An SCLH of 236 (0xec), yielding a t_sclh of 4740 ns
            // - An SCLL of 255 (0xff), yielding a t_scll of 5120 ns
            //
            // Taken together, this yields a t_scl of 9860 ns, which (as
            // above) when added to t_sync1 and t_sync2 will be close to our
            // target of 10000 ns.  Finally, we set SCLDEL to 12 and SDADEL to
            // 0 -- values that come from from the STM32CubeMX tool.
            i2c.timingr.write(|w| { w
                .presc().bits(1)
                .sclh().bits(236)
                .scll().bits(255)
                .scldel().bits(12)
                .sdadel().bits(0)
            });
        } else {
            compile_error!("unknown STM32H7 variant");
        }
    }

    // WTALF?!
    i2c.oar1.write(|w| { w.oa1en().clear_bit() });
    i2c.oar1.write(|w| { w
        .oa1en().set_bit()
        .oa1mode().clear_bit()
        .oa1().bits(0)
    });

    i2c.cr2.write(|w| { w.autoend().set_bit().nack().set_bit() });

    i2c.oar2.write(|w| { w.oa2en().clear_bit() });
    i2c.oar2.write(|w| { w
        .oa2en().set_bit()
        .oa2().bits(0)
    });

    i2c.cr1.modify(|_, w| { w
        .gcen().clear_bit()             // disable General Call
        .nostretch().clear_bit()        // disable clock stretching
        .errie().set_bit()              // emable Error Interrupt
        .tcie().set_bit()               // enable Transfer Complete interrupt
        .stopie().set_bit()             // enable Stop Detection interrupt
        .nackie().set_bit()             // enable NACK interrupt
        .rxie().set_bit()               // enable RX interrupt
        .txie().set_bit()               // enable TX interrupt
    });

    i2c.cr1.modify(|_, w| { w.pe().set_bit() });
}

fn configure_controllers() {
    let mut controllers = unsafe { &mut I2C_CONTROLLERS };

    for controller in controllers {
        controller.registers = Some(unsafe { &*(controller.getblock)() });
        configure_controller(controller.registers.unwrap());
        sys_irq_control(controller.notification, true);
    }
}

fn configure_port(
    controller: &mut I2cController,
    pin: &I2cPin,
) {
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
        let controller = lookup_controller(pin.controller as u8).ok().unwrap();

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
                        pin.function
                    )
                .unwrap();
                controller.port = Some(pin.port);
            }
            Some(_) => {}
        }
    }
}

fn write_read(
    i2c: &RegisterBlock,
    notification: u32,
    addr: u8,
    wlen: usize,
    getbyte: impl Fn(usize) -> Option<u8>,
    rlen: usize,
    putbyte: impl Fn(usize, u8) -> Option<()>,
) -> Result<(), ResponseCode> {
    if wlen == 0 && rlen == 0 {
        // We must have either a write OR a read -- while perhaps valid to
        // support both being zero as a way of testing an address for a
        // NACK, it's not a mode that we (currently) support.
        return Err(ResponseCode::BadArg);
    }

    if wlen > 255 || rlen > 255 {
        // For now, we don't support writing or reading more than 255 bytes
        return Err(ResponseCode::BadArg);
    }

    // Before we talk to the controller, spin until it isn't busy
    loop {
        let isr = i2c.isr.read();

        if !isr.busy().is_busy() {
            break;
        }
    }

    if wlen > 0 {
        i2c.cr2.modify(|_, w| { w
            .nbytes().bits(wlen as u8)
            .autoend().clear_bit()
            .add10().clear_bit()
            .sadd().bits((addr << 1).into())
            .rd_wrn().clear_bit()
            .start().set_bit()
        });

        let mut pos = 0;

        while pos < wlen {
            loop {
                let isr = i2c.isr.read();

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| { w.nackcf().set_bit() });
                    return Err(ResponseCode::NoDevice);
                }

                if isr.txis().is_empty() {
                    break;
                }

                let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
                sys_irq_control(notification, true);
            }

            // Get a single byte
            let byte: u8 = getbyte(pos).ok_or(ResponseCode::BadArg)?;

            // And send it!
            i2c.txdr.write(|w| w.txdata().bits(byte));
            pos += 1;
        }

        // All done; now spin until our transfer is complete -- or until
        // we've been NACK'd (denoting an illegal register value)
        loop {
            let isr = i2c.isr.read();

            if isr.nackf().is_nack() {
                i2c.icr.write(|w| { w.nackcf().set_bit() });
                return Err(ResponseCode::NoRegister);
            }

            if isr.tc().is_complete() {
                break;
            }

            let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
            sys_irq_control(notification, true);
        }
    }

    if rlen > 0 {
        //
        // If we have both a write and a read, we deliberately do not send
        // a STOP between them to force the RESTART (many devices do not
        // permit a STOP between a register address write and a subsequent
        // read).
        //
        i2c.cr2.modify(|_, w| { w
            .nbytes().bits(rlen as u8)
            .autoend().clear_bit()
            .add10().clear_bit()
            .sadd().bits((addr << 1).into())
            .rd_wrn().set_bit()
            .start().set_bit()
        });

        let mut pos = 0;

        while pos < rlen {
            loop {
                let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
                sys_irq_control(notification, true);

                let isr = i2c.isr.read();

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| { w.nackcf().set_bit() });
                    return Err(ResponseCode::NoDevice);
                }

                if !isr.rxne().is_empty() {
                    break;
                }
            }

            // Read it!
            let byte: u8 = i2c.rxdr.read().rxdata().bits();
            putbyte(pos, byte).ok_or(ResponseCode::BadArg)?;
            pos += 1;
        }

        // All done; now spin until our transfer is complete...
        while !i2c.isr.read().tc().is_complete() {
            let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
            sys_irq_control(notification, true);
        }
    }

    //
    // Whether we did a write alone, a read alone, or a write followed
    // by a read, we're done now -- manually send a STOP.
    //
    i2c.cr2.modify(|_, w| { w.stop().set_bit() });

    Ok(())
}
