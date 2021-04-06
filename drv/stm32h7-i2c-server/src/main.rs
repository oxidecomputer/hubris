//! A driver for the STM32H7 I2C interface

#![no_std]
#![no_main]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

use drv_i2c_api::Port;
use drv_i2c_api::*;
use drv_stm32h7_gpio_api::*;
use drv_stm32h7_i2c::*;
use drv_stm32h7_rcc_api::{Peripheral, Rcc};

use fixedmap::*;
use ringbuf::*;
use userlib::*;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;

#[cfg(feature = "standalone")]
const RCC: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const GPIO: Task = Task::gpio_driver;

#[cfg(feature = "standalone")]
const GPIO: Task = Task::anonymous;

fn lookup_controller<'a>(
    controllers: &'a [I2cController],
    controller: Controller,
) -> Result<&'a I2cController<'a>, ResponseCode> {
    for i in 0..controllers.len() {
        if controllers[i].controller == controller {
            return Ok(&controllers[i]);
        }
    }

    Err(ResponseCode::BadController)
}

fn lookup_pin<'a>(
    pins: &'a [I2cPin],
    controller: Controller,
    port: Port,
) -> Result<&'a I2cPin, ResponseCode> {
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

fn find_mux(
    controller: &I2cController,
    port: Port,
    muxes: &[I2cMux],
    mux: Option<(Mux, Segment)>,
    mut func: impl FnMut(&I2cMux, Mux, Segment) -> Result<(), ResponseCode>,
) -> Result<(), ResponseCode> {
    match mux {
        Some((id, segment)) => {
            for mux in muxes {
                if mux.controller != controller.controller {
                    continue;
                }

                if mux.port != port || mux.id != id {
                    continue;
                }

                return func(mux, id, segment);
            }

            Err(ResponseCode::MuxNotFound)
        }
        None => Ok(()),
    }
}

fn configure_mux(
    map: &mut MuxMap,
    controller: &I2cController,
    port: Port,
    mux: Option<(Mux, Segment)>,
    muxes: &[I2cMux],
    ctrl: &I2cControl,
) -> Result<(), ResponseCode> {
    find_mux(controller, port, muxes, mux, |mux, id, segment| {
        // Determine if the current segment matches our specified segment...
        if let Some(current) = map.get(id) {
            if current == segment {
                return Ok(());
            }

            // Beyond this point, we want any failure to set our new
            // segment to leave our segment unset rather than having
            // it point to the old segment.
            map.remove(id);
        }

        // If we're here, our mux is valid, but the current segment is
        // not the specfied segment; we will now call upon our
        // driver to enable this segment.
        mux.driver.enable_segment(mux, controller, segment, ctrl)?;
        map.insert(id, segment);

        Ok(())
    })
}

ringbuf!(Option<ResponseCode>, 16, None);

fn reset_if_needed(
    code: ResponseCode,
    controller: &I2cController,
    port: Port,
    muxes: &[I2cMux],
    mux: Option<(Mux, Segment)>,
) {
    ringbuf_entry!(Some(code));

    match code {
        ResponseCode::BusLocked
        | ResponseCode::BusLockedMux
        | ResponseCode::BusReset
        | ResponseCode::BusResetMux => {}
        _ => {
            return;
        }
    }

    let gpio = TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio = Gpio::from(gpio);

    // First, bounce our I2C controller
    controller.reset();

    // And now reset the mux, eating any errors.
    let _ = find_mux(controller, port, muxes, mux, |mux, _, _| {
        ringbuf_entry!(None);
        mux.driver.reset(&mux, &gpio)?;
        Ok(())
    });
}

type PortMap = FixedMap<Controller, Port, 8>;
type MuxMap = FixedMap<Mux, Segment, 2>;

#[export_name = "main"]
fn main() -> ! {
    cfg_if::cfg_if! {
        if #[cfg(target_board = "stm32h7b3i-dk")] {
            let controllers = [ I2cController {
                controller: Controller::I2C4,
                peripheral: Peripheral::I2c4,
                notification: (1 << (4 - 1)),
                registers: unsafe { &*device::I2C4::ptr() },
            } ];

            let pins = [ I2cPin {
                controller: Controller::I2C4,
                port: Port::D,
                gpio_port: drv_stm32h7_gpio_api::Port::D,
                function: Alternate::AF4,
                mask: (1 << 12) | (1 << 13),
            } ];

            let muxes = [];
        } else if #[cfg(target_board = "nucleo-h743zi2")] {
            let controllers = [ I2cController {
                controller: Controller::I2C2,
                peripheral: Peripheral::I2c2,
                notification: (1 << (2 - 1)),
                registers: unsafe { &*device::I2C2::ptr() },
            } ];

            let pins = [ I2cPin {
                controller: Controller::I2C2,
                port: Port::F,
                gpio_port: drv_stm32h7_gpio_api::Port::F,
                function: Alternate::AF4,
                mask: (1 << 0) | (1 << 1),
            } ];

            let muxes = [ I2cMux {
                controller: Controller::I2C2,
                port: Port::F,
                id: Mux::M1,
                driver: &max7358::Max7358,
                enable: None,
                address: 0x70,
            } ];
        } else if #[cfg(target_board = "gemini-bu-1")] {
            let controllers = [ I2cController {
                controller: Controller::I2C1,
                peripheral: Peripheral::I2c1,
                notification: (1 << (1 - 1)),
                registers: unsafe { &*device::I2C1::ptr() },
            }, I2cController {
                controller: Controller::I2C3,
                peripheral: Peripheral::I2c3,
                notification: (1 << (3 - 1)),
                registers: unsafe { &*device::I2C3::ptr() },
            }, I2cController {
                controller: Controller::I2C4,
                peripheral: Peripheral::I2c4,
                notification: (1 << (4 - 1)),
                registers: unsafe { &*device::I2C4::ptr() },
            } ];

            let pins = [ I2cPin {
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

            let muxes = [ I2cMux {
                controller: Controller::I2C4,
                port: Port::F,
                id: Mux::M1,
                driver: &drv_stm32h7_i2c::ltc4306::Ltc4306,
                enable: Some(I2cPin {
                    controller: Controller::I2C4,
                    port: Port::Default,
                    gpio_port: drv_stm32h7_gpio_api::Port::G,
                    function: Alternate::AF0,
                    mask: (1 << 0),
                }),
                address: 0x44,
            }, I2cMux {
                controller: Controller::I2C4,
                port: Port::D,
                id: Mux::M1,
                driver: &drv_stm32h7_i2c::max7358::Max7358,
                enable: None,
                address: 0x70,
            } ];
        } else {
            compile_error!("no I2C controllers/pins for unknown board");
        }
    }

    // This is our actual mutable state
    let mut portmap = PortMap::new();
    let mut muxmap = MuxMap::new();

    // Turn the actual peripheral on so that we can interact with it.
    turn_on_i2c(&controllers);
    configure_pins(&controllers, &pins, &mut portmap);
    configure_controllers(&controllers);

    // Field messages.
    let mut buffer = [0; 4];

    let ctrl = I2cControl {
        enable: |notification| {
            sys_irq_control(notification, true);
        },
        wfi: |notification| {
            let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
        },
    };

    configure_muxes(&muxes, &controllers, &pins, &mut portmap, &ctrl);

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

                let controller = lookup_controller(&controllers, controller)?;
                let pin = lookup_pin(&pins, controller.controller, port)?;

                configure_port(&mut portmap, controller, pin, &pins);

                match configure_mux(
                    &mut muxmap,
                    controller,
                    pin.port,
                    mux,
                    &muxes,
                    &ctrl,
                ) {
                    Ok(_) => {}
                    Err(code) => {
                        reset_if_needed(
                            code, controller, pin.port, &muxes, mux,
                        );
                        return Err(code);
                    }
                }

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
                    |pos| wbuf.read_at(pos),
                    rinfo.len,
                    |pos, byte| rbuf.write_at(pos, byte),
                    &ctrl,
                ) {
                    Err(code) => {
                        reset_if_needed(code, controller, port, &muxes, mux);
                        Err(code)
                    }
                    Ok(_) => {
                        caller.reply(());
                        Ok(())
                    }
                }
            }
        });
    }
}

fn turn_on_i2c(controllers: &[I2cController]) {
    let rcc_driver = Rcc::from(TaskId::for_index_and_gen(
        RCC as usize,
        Generation::default(),
    ));

    for controller in controllers {
        controller.enable(&rcc_driver);
    }
}

fn configure_controllers(controllers: &[I2cController]) {
    for controller in controllers {
        controller.configure();
        sys_irq_control(controller.notification, true);
    }
}

fn configure_port(
    map: &mut PortMap,
    controller: &I2cController,
    pin: &I2cPin,
    pins: &[I2cPin],
) {
    let p = map.get(controller.controller).unwrap();

    if p == pin.port {
        return;
    }

    let gpio = TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio = Gpio::from(gpio);

    let src = lookup_pin(pins, controller.controller, p).ok().unwrap();

    gpio.configure(
        src.gpio_port,
        src.mask,
        Mode::Alternate,
        OutputType::OpenDrain,
        Speed::High,
        Pull::None,
        Alternate::AF0,
    )
    .unwrap();

    gpio.configure(
        pin.gpio_port,
        pin.mask,
        Mode::Alternate,
        OutputType::OpenDrain,
        Speed::High,
        Pull::None,
        pin.function,
    )
    .unwrap();

    map.insert(controller.controller, pin.port);
}

fn configure_pins(
    controllers: &[I2cController],
    pins: &[I2cPin],
    map: &mut PortMap,
) {
    let gpio = TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio = Gpio::from(gpio);

    for pin in pins {
        let controller =
            lookup_controller(controllers, pin.controller).ok().unwrap();

        match map.get(controller.controller) {
            None => {
                gpio.configure(
                    pin.gpio_port,
                    pin.mask,
                    Mode::Alternate,
                    OutputType::OpenDrain,
                    Speed::High,
                    Pull::None,
                    pin.function,
                )
                .unwrap();
                map.insert(controller.controller, pin.port);
            }
            Some(_) => {}
        }
    }
}

fn configure_muxes(
    muxes: &[I2cMux],
    controllers: &[I2cController],
    pins: &[I2cPin],
    map: &mut PortMap,
    ctrl: &I2cControl,
) {
    let gpio = TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio = Gpio::from(gpio);

    for mux in muxes {
        let controller =
            lookup_controller(controllers, mux.controller).unwrap();
        let pin = lookup_pin(&pins, mux.controller, mux.port).unwrap();

        configure_port(map, controller, pin, pins);
        let _ = mux.driver.configure(&mux, controller, &gpio, ctrl);
    }
}
