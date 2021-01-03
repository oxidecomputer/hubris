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

    // And actually configure ourselves as a target
    let notification = controller.notification;

    let wfi = || {
        let _ = sys_recv_closed(&mut [], notification, TaskId::KERNEL);
    };

    let response = |register| -> Option<&[u8]> {
        match register {
            Some(0) => { Some(&[ 0xbc ]) }
            Some(1) => { Some(&[ 0xbd ]) }
            Some(2) => { Some(&[ 0xbe ]) }
            // Some(0xbc) => { Some(&[ 0x4f, 0x78 ]) }
            _ => { Some(&[ 0x55 ]) }
            // None
        }
    };

    controller.configure_as_target(0x1d);

    let i2c = controller.registers.unwrap();
    sys_irq_control(notification, true);

    let mut register = None;

    'addrloop: loop {
        let is_write = loop {
            let isr = i2c.isr.read();

            if isr.stopf().is_stop() {
                i2c.icr.write(|w| { w.stopcf().set_bit() });
                continue;
            }

            if isr.addr().is_match_() {
                break isr.dir().is_write();
            }

            wfi();
            sys_irq_control(notification, true);
        };

        // Clear our Address interrupt
        i2c.icr.write(|w| { w.addrcf().set_bit() });

        if is_write {
            i2c.cr2.modify(|_, w| { w.nbytes().bits(1) });
            'rxloop: loop {
                let isr = i2c.isr.read();

                if isr.addr().is_match_() {
                    //
                    // If we have an address match, check to see if this is
                    // change in direction; if it is, break out of our receive
                    // loop.
                    //
                    if !isr.dir().is_write() {
                        i2c.icr.write(|w| { w.addrcf().set_bit() });
                        break 'rxloop;
                    }

                    i2c.icr.write(|w| { w.addrcf().set_bit() });
                    continue 'rxloop;
                }

                if isr.stopf().is_stop() {
                    i2c.icr.write(|w| { w.stopcf().set_bit() });
                    break 'rxloop;
                }

                if isr.nackf().is_nack() {
                    i2c.icr.write(|w| { w.nackcf().set_bit() });
                    break 'rxloop;
                }

                if isr.rxne().is_not_empty() {
                    //
                    // We have a byte; we'll read it, and continue to wait
                    // for additional bytes.
                    //
                    register = Some(i2c.rxdr.read().rxdata().bits());
                    continue 'rxloop;
                }

                wfi();
                sys_irq_control(notification, true);
            }
        }

        let wbuf = match response(register) {
            None => {
                //
                // We have read from an invalid register; NACK it
                //
                i2c.cr2.modify(|_, w| { w
                    .nbytes().bits(0)
                    .nack().set_bit()
                });
                continue 'addrloop;
            }
            Some(wbuf) => wbuf
        };

        // This is a read from the controller.  Because SBC is set, we must
        // indicate the number of bytes that we will send.
        i2c.cr2.modify(|_, w| { w.nbytes().bits(wbuf.len() as u8) });
        let mut pos = 0;

        'txloop: loop {
            let isr = i2c.isr.read();

            if isr.tc().is_complete() {
                //
                // We're done -- write the stop bit, and kick out to our
                // address loop.
                //
                i2c.cr2.modify(|_, w| { w.stop().set_bit() });
                continue 'addrloop;
            }

            if isr.addr().is_match_() {
                //
                // We really aren't expecting this, so kick out to the top
                // of the loop to try to make sense of it.
                //
                continue 'addrloop;
            }

            if isr.txis().is_empty() {
                if pos < wbuf.len() {
                    sys_log!("txloop: sending 0x{:x}", wbuf[pos]);
                    i2c.txdr.write(|w| { w.txdata().bits(wbuf[pos]) });
                    pos += 1;
                    continue 'txloop;
                } else {
                    //
                    // We're not really expecting this -- NACK and kick
                    // out.
                    //
                    i2c.cr2.modify(|_, w| { w.nack().set_bit() });
                    continue 'addrloop;
                }
            }

            if isr.nackf().is_nack() {
                i2c.icr.write(|w| { w.nackcf().set_bit() });
                continue 'addrloop;
            }

            wfi();
            sys_irq_control(notification, true);
        }
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
