//! A driver for the STM32H7 I2C interface

#![no_std]
#![no_main]

#[cfg(feature = "h7b3")]
use stm32h7::stm32h7b3 as device;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

use drv_i2c_api::Port;
use drv_i2c_api::*;
use drv_stm32h7_gpio_api::{
    self as gpio_api, Alternate, Gpio, OutputType, Pull, Speed,
};
use drv_stm32h7_i2c::*;
use drv_stm32h7_rcc_api::{Peripheral, Rcc};

use fixedmap::*;
use ringbuf::*;
use userlib::*;

task_slot!(RCC, rcc_driver);
task_slot!(GPIO, gpio_driver);

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

///
/// Validates a port for the specified controller, translating `Port::Default`
/// to the matching port (or returning an error if there is more than one port
/// and `Port::Default` has been specified).
///
fn validate_port<'a>(
    pins: &'a [I2cPin],
    controller: Controller,
    port: Port,
) -> Result<Port, ResponseCode> {
    if port != Port::Default {
        //
        // The more straightforward case is when our port has been explicitly
        // provided -- we just need to verify that it's valid.
        //
        pins.iter()
            .find(|pin| pin.controller == controller && pin.port == port)
            .map(|pin| pin.port)
            .ok_or(ResponseCode::BadPort)
    } else {
        let mut found = pins
            .iter()
            .filter(|pin| pin.controller == controller)
            .map(|pin| pin.port);

        //
        // A default port has been requested; we need to verify that there is
        // but one port for this controller -- if there is more than one, we
        // require the port to be explicitly provided.
        //
        match found.next() {
            None => Err(ResponseCode::BadController),
            Some(port) => {
                if found.any(|p| p != port) {
                    Err(ResponseCode::BadDefaultPort)
                } else {
                    Ok(port)
                }
            }
        }
    }
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
        | ResponseCode::BusResetMux
        | ResponseCode::ControllerLocked => {}
        _ => {
            return;
        }
    }

    let gpio = GPIO.get_task_id();
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
                gpio_pins: gpio_api::Port::D.pin(12).and_pin(13),
                function: Alternate::AF4,
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
                gpio_pins: gpio_api::Port::F.pin(0).and_pin(1),
                function: Alternate::AF4,
            } ];

            let muxes = [
            #[cfg(feature = "external-spd")]
            I2cMux {
                controller: Controller::I2C2,
                port: Port::F,
                id: Mux::M1,
                driver: &drv_stm32h7_i2c::ltc4306::Ltc4306,
                enable: None,
                address: 0b1001_010,
            },

            #[cfg(feature = "external-max7358")]
            I2cMux {
                controller: Controller::I2C2,
                port: Port::F,
                id: Mux::M1,
                driver: &drv_stm32h7_i2c::max7358::Max7358,
                enable: None,
                address: 0x70,
            },

            ];
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
                gpio_pins: gpio_api::Port::B.pin(8).and_pin(9),
                function: Alternate::AF4,
            }, I2cPin {
                controller: Controller::I2C4,
                port: Port::D,
                gpio_pins: gpio_api::Port::D.pin(12).and_pin(13),
                function: Alternate::AF4,
            }, I2cPin {
                controller: Controller::I2C4,
                port: Port::F,
                gpio_pins: gpio_api::Port::F.pin(14).and_pin(15),
                function: Alternate::AF4,
            }, I2cPin {
                controller: Controller::I2C3,
                port: Port::H,
                gpio_pins: gpio_api::Port::H.pin(7).and_pin(8),
                function: Alternate::AF4,
            }, I2cPin {
                controller: Controller::I2C4,
                port: Port::H,
                gpio_pins: gpio_api::Port::H.pin(11).and_pin(12),
                function: Alternate::AF4,
            } ];

            let muxes = [
                I2cMux {
                controller: Controller::I2C4,
                port: Port::F,
                id: Mux::M1,
                driver: &drv_stm32h7_i2c::ltc4306::Ltc4306,
                enable: Some(I2cPin {
                    controller: Controller::I2C4,
                    port: Port::Default,
                    gpio_pins: gpio_api::Port::G.pin(0),
                    function: Alternate::AF0,
                }),
                address: 0x44,
            },
            #[cfg(feature = "external-max7358")]
            I2cMux {
                controller: Controller::I2C4,
                port: Port::D,
                id: Mux::M1,
                driver: &drv_stm32h7_i2c::max7358::Max7358,
                enable: None,
                address: 0x70,
            },

            #[cfg(feature = "external-pca9548")]
            I2cMux {
                controller: Controller::I2C4,
                port: Port::H,
                id: Mux::M1,
                driver: &drv_stm32h7_i2c::pca9548::Pca9548,
                enable: None,
                address: 0x70,
            },

            ];
        } else if #[cfg(target_board = "gimletlet-2")] {
            let controllers = [

            #[cfg(not(feature = "target-enable"))]
            I2cController {
                controller: Controller::I2C2,
                peripheral: Peripheral::I2c2,
                notification: (1 << (2 - 1)),
                registers: unsafe { &*device::I2C2::ptr() },
            },

            I2cController {
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

            //
            // Note that I2C3 is a bit unusual in that its SCL and SDA are on
            // two different ports (port A and port C, respectively); we
            // therefore have two `I2cPin` structures for I2C3, but for
            // purposes of the abstraction that we export to consumers, we
            // call the pair logical port A.
            //
            let pins = [
            #[cfg(not(feature = "target-enable"))]
            I2cPin {
                controller: Controller::I2C2,
                port: Port::F,
                gpio_pins: gpio_api::Port::F.pin(0).and_pin(1),
                function: Alternate::AF4,
            },

            I2cPin {
                controller: Controller::I2C3,
                port: Port::A,
                gpio_pins: gpio_api::Port::A.pin(8),
                function: Alternate::AF4,
            }, I2cPin {
                controller: Controller::I2C3,
                port: Port::A,
                gpio_pins: gpio_api::Port::C.pin(9),
                function: Alternate::AF4,
            }, I2cPin {
                controller: Controller::I2C4,
                port: Port::F,
                gpio_pins: gpio_api::Port::F.pin(14).and_pin(15),
                function: Alternate::AF4,
            } ];

            let muxes = [];
        } else if #[cfg(target_board = "gimlet-1")] {
            // SP3 proxy is handled in task-spd

            let controllers = [
                // Front M.2
                I2cController {
                    controller: Controller::I2C2,
                    peripheral: Peripheral::I2c2,
                    notification: (1 << (2 - 1)),
                    registers: unsafe { &*device::I2C2::ptr() },
                },
                // Mid
                I2cController {
                    controller: Controller::I2C3,
                    peripheral: Peripheral::I2c3,
                    notification: (1 << (3 - 1)),
                    registers: unsafe { &*device::I2C3::ptr() },
                },
                // Rear
                I2cController {
                    controller: Controller::I2C4,
                    peripheral: Peripheral::I2c4,
                    notification: (1 << (4 - 1)),
                    registers: unsafe { &*device::I2C4::ptr() },
            } ];


            let pins = [
                // Note we have two different sets of pins on two different
                // ports for I2C2!

                // SMBUS_SP_TO_LVL_FRONT_SMDAT
                // SMBUS_SP_TO_LVL_FRONT_SMCLK
                I2cPin {
                    controller: Controller::I2C2,
                    port: Port::F,
                    gpio_pins: gpio_api::Port::F.pin(0).and_pin(1),
                    function: Alternate::AF4,
                },

                // SMBUS_SP_TO_M2_SMCLK_A2_V3P3
                // SMBUS_SP_TO_M2_SMDAT_A2_V3P3
                I2cPin {
                    controller: Controller::I2C2,
                    port: Port::B,
                    gpio_pins: gpio_api::Port::B.pin(10).and_pin(11),
                    function: Alternate::AF4,
                },

                // SMBUS_SP_TO_LVL_MID_SMCLK
                // SMBUS_SP_TO_LVL_MID_SMDAT
                I2cPin {
                    controller: Controller::I2C3,
                    port: Port::H,
                    gpio_pins: gpio_api::Port::H.pin(7).and_pin(8),
                    function: Alternate::AF4,
                },
                // SMBUS_SP_TO_LVL_REAR_SMCLK
                // SMBUS_SP_TO_LVL_REAR_SMDAT
                I2cPin {
                    controller: Controller::I2C4,
                    port: Port::F,
                    gpio_pins: gpio_api::Port::F.pin(14).and_pin(15),
                    function: Alternate::AF4,
                },
            ];

            let muxes = [
                // Front muxes for
                // SMBUS_SP_TO_FRONT_SMCLK_A2_V3P3
                // SMBUS_SP_TO_FRONT_SMDAT_A2_V3P3
                I2cMux {
                    controller: Controller::I2C2,
                    port: Port::F,
                    id: Mux::M1,
                    driver: &drv_stm32h7_i2c::pca9548::Pca9548,
                    enable: None,
                    address: 0x70,
                },

                I2cMux {
                    controller: Controller::I2C2,
                    port: Port::F,
                    id: Mux::M1,
                    driver: &drv_stm32h7_i2c::pca9548::Pca9548,
                    enable: None,
                    address: 0x71,
                },

                I2cMux {
                    controller: Controller::I2C2,
                    port: Port::F,
                    id: Mux::M1,
                    driver: &drv_stm32h7_i2c::pca9548::Pca9548,
                    enable: None,
                    address: 0x72,
                },

                // M.2 mux on
                // SMBUS_SP_TO_M2_SMCLK_A2_V3P3
                // SMBUS_SP_TO_M2_SMDAT_A2_V3P3
                I2cMux {
                    controller: Controller::I2C2,
                    port: Port::B,
                    id: Mux::M1,
                    driver: &drv_stm32h7_i2c::pca9548::Pca9548,
                    enable: None,
                    address: 0x73,
                },
            ];
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
            Op::WriteRead | Op::WriteReadBlock => {
                let (payload, caller) = msg
                    .fixed_with_leases::<[u8; 4], usize>(2)
                    .ok_or(ResponseCode::BadArg)?;

                let (addr, controller, port, mux) =
                    Marshal::unmarshal(payload)?;

                if let Some(_) = ReservedAddress::from_u8(addr) {
                    return Err(ResponseCode::ReservedAddress);
                }

                let controller = lookup_controller(&controllers, controller)?;
                let port = validate_port(&pins, controller.controller, port)?;

                configure_port(&mut portmap, controller, port, &pins);

                match configure_mux(
                    &mut muxmap,
                    controller,
                    port,
                    mux,
                    &muxes,
                    &ctrl,
                ) {
                    Ok(_) => {}
                    Err(code) => {
                        reset_if_needed(code, controller, port, &muxes, mux);
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

                let mut nread = 0;

                match controller.write_read(
                    addr,
                    winfo.len,
                    |pos| wbuf.read_at(pos),
                    if op == Op::WriteRead {
                        ReadLength::Fixed(rinfo.len)
                    } else {
                        ReadLength::Variable
                    },
                    |pos, byte| {
                        if pos + 1 > nread {
                            nread = pos + 1;
                        }

                        rbuf.write_at(pos, byte)
                    },
                    &ctrl,
                ) {
                    Err(code) => {
                        reset_if_needed(code, controller, port, &muxes, mux);
                        Err(code)
                    }
                    Ok(_) => {
                        caller.reply(nread);
                        Ok(())
                    }
                }
            }
        });
    }
}

fn turn_on_i2c(controllers: &[I2cController]) {
    let rcc_driver = Rcc::from(RCC.get_task_id());

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
    port: Port,
    pins: &[I2cPin],
) {
    let current = map.get(controller.controller).unwrap();

    assert!(port != Port::Default);

    if current == port {
        return;
    }

    let gpio = GPIO.get_task_id();
    let gpio = Gpio::from(gpio);

    //
    // We will now iterate over all pins, de-configuring any that match our
    // old port, and configuring any that match our new port.
    //
    for pin in pins
        .iter()
        .filter(|p| p.controller == controller.controller)
    {
        if pin.port == current {
            //
            // We de-configure our current port by setting the pins to
            // `Mode::input`, which will assure that we don't leave SCL and
            // SDA pulled high.
            //
            gpio.configure_input(pin.gpio_pins, Pull::None).unwrap();
        } else if pin.port == port {
            // Configure our new port!
            gpio.configure_alternate(
                pin.gpio_pins,
                OutputType::OpenDrain,
                Speed::High,
                Pull::None,
                pin.function,
            )
            .unwrap();
        }
    }

    map.insert(controller.controller, port);
}

fn configure_pins(
    controllers: &[I2cController],
    pins: &[I2cPin],
    map: &mut PortMap,
) {
    let gpio = GPIO.get_task_id();
    let gpio = Gpio::from(gpio);

    for pin in pins {
        let controller =
            lookup_controller(controllers, pin.controller).ok().unwrap();

        match map.get(controller.controller) {
            Some(port) if port != pin.port => {
                //
                // If we have already enabled this controller with a different
                // port, we don't want to enable this pin.
                //
                continue;
            }
            _ => {}
        }

        gpio.configure_alternate(
            pin.gpio_pins,
            OutputType::OpenDrain,
            Speed::High,
            Pull::None,
            pin.function,
        )
        .unwrap();

        map.insert(controller.controller, pin.port);
    }
}

fn configure_muxes(
    muxes: &[I2cMux],
    controllers: &[I2cController],
    pins: &[I2cPin],
    map: &mut PortMap,
    ctrl: &I2cControl,
) {
    let gpio = GPIO.get_task_id();
    let gpio = Gpio::from(gpio);

    for mux in muxes {
        let controller =
            lookup_controller(controllers, mux.controller).unwrap();
        configure_port(map, controller, mux.port, pins);

        loop {
            match mux.driver.configure(&mux, controller, &gpio, ctrl) {
                Ok(_) => {
                    break;
                }
                Err(code) => {
                    ringbuf_entry!(Some(code));
                    reset_if_needed(code, controller, mux.port, muxes, None);
                }
            }
        }
    }
}
