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
use drv_i2c_api::{Interface, Op, ReservedAddress};
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

#[repr(u32)]
enum ResponseCode {
    BadArg = 1,
    NoDevice = 2,
    Busy = 3,
    BadInterface = 4,
    ReservedAddress = 5,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

type GetBlock = fn() -> *const RegisterBlock;

cfg_if::cfg_if! {
    if #[cfg(target_board = "stm32h7b3i-dk")] {
        const I2C_BUSSES: [(Interface, Peripheral, GetBlock); 1] = [
            (Interface::I2C4, Peripheral::I2c4, device::I2C4::ptr),
        ];

        const I2C_PINS: [(Port, Alternate, u16); 1] = [
            (Port::D, Alternate::AF4, (1 << 12) | (1 << 13))
        ];
    } else if #[cfg(target_board = "gemini-bu-1")] {
        const I2C_BUSSES: [(Interface, Peripheral, GetBlock); 4] = [
            (Interface::I2C1, Peripheral::I2c1, device::I2C1::ptr),
            (Interface::I2C2, Peripheral::I2c2, device::I2C2::ptr),
            (Interface::I2C3, Peripheral::I2c3, device::I2C3::ptr),
            (Interface::I2C4, Peripheral::I2c4, device::I2C4::ptr),
        ];

        const I2C_PINS: [(Port, Alternate, u16); 6] = [
            (Port::B, Alternate::AF4, (1 << 8) | (1 << 9)),     // I2C1
            (Port::D, Alternate::AF4, (1 << 12) | (1 << 13)),   // I2C4
            (Port::F, Alternate::AF4, (1 << 0) | (1 << 1)),     // I2C2
            (Port::F, Alternate::AF4, (1 << 14) | (1 << 15)),   // I2C4
            (Port::H, Alternate::AF4, (1 << 7) | (1 << 8)),     // I2C3
            (Port::H, Alternate::AF4, (1 << 11) | (1 << 12)),   // I2C4
        ];
    } else {
        compile_error!("no I2C busses/pins for unknown board");
    }
}

fn bus(interface: u8) -> Option<GetBlock> {
    match Interface::from_u8(interface) {
        Some(interface) => {
            for i in &I2C_BUSSES {
                if interface == i.0 {
                    return Some(i.2);
                }
            }
            None
        }
        _ => {
            None
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_i2c();
    configure_pins();
    configure_busses();

    // Field messages.
    let mut buffer = [0; 2];

    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::WriteRead => {
                let (&[addr, interface], caller) = msg
                    .fixed_with_leases::<[u8; 2], ()>(2)
                    .ok_or(ResponseCode::BadArg)?;

                let i2cp = bus(interface).ok_or(ResponseCode::BadInterface)?;
                let i2c = unsafe { &*i2cp() };
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

                write_read(
                    i2c,
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
    let rcc_driver = Rcc::from(TaskId::for_index_and_gen(
        RCC as usize,
        Generation::default(),
    ));

    for bus in &I2C_BUSSES {
        rcc_driver.enable_clock(bus.1);
        rcc_driver.leave_reset(bus.1);
    }
}

fn configure_bus(i2c: &RegisterBlock) {
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

    i2c.cr1.write(|w| { w
        .gcen().clear_bit()
        .nostretch().clear_bit()
    });

    i2c.cr1.write(|w| { w.pe().set_bit() });
}

fn configure_busses() {
    for bus in &I2C_BUSSES {
        let i2c = unsafe { &*bus.2() };
        configure_bus(i2c);
    }
}

fn configure_pins() {
    let gpio_driver =
        TaskId::for_index_and_gen(GPIO as usize, Generation::default());
    let gpio_driver = Gpio::from(gpio_driver);

    for pin in &I2C_PINS {
        gpio_driver
            .configure(
                pin.0,
                pin.2,
                Mode::Alternate,
                OutputType::OpenDrain,
                Speed::High,
                Pull::None,
                pin.1
            )
            .unwrap();
    }
}

fn write_read(
    i2c: &RegisterBlock,
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
            }

            // Get a single byte
            let byte: u8 = getbyte(pos).ok_or(ResponseCode::BadArg)?;

            // And send it!
            i2c.txdr.write(|w| w.txdata().bits(byte));
            pos += 1;
        }

        // All done; now spin until our transfer is complete...
        while !i2c.isr.read().tc().is_complete() {
            continue;
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
            continue;
        }
    }

    //
    // Whether we did a write alone, a read alone, or a write followed
    // by a read, we're done now -- manually send a STOP.
    //
    i2c.cr2.modify(|_, w| { w.stop().set_bit() });

    Ok(())
}
