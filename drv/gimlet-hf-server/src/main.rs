//! Gimlet host flash server.
//!
//! This server is responsible for managing access to the host flash; it embeds
//! the QSPI flash driver.

#![no_std]
#![no_main]

use userlib::*;

use drv_stm32h7_gpio_api as gpio_api;
use drv_stm32h7_qspi::Qspi;
use drv_stm32h7_rcc_api as rcc_api;
use stm32h7::stm32h743 as device;

declare_task!(RCC, rcc_driver);
declare_task!(GPIO, gpio_driver);

/// Operations in our IPC interface.
#[derive(FromPrimitive)]
enum Op {
    ReadId = 1,
    ReadStatus = 2,
    BulkErase = 3,
    PageProgram = 4,
    Read = 5,
    SectorErase = 6,
}

/// Errors from our IPC interface.
enum HfError {
    Bad = 1,
    WriteEnableFailed = 2,
    MissingLease = 3,
    BadLease = 4,
}

impl From<HfError> for u32 {
    fn from(e: HfError) -> u32 {
        e as u32
    }
}

#[export_name = "main"]
fn main() -> ! {
    let rcc_driver = rcc_api::Rcc::from(get_task_id(RCC));
    let gpio_driver = gpio_api::Gpio::from(get_task_id(GPIO));

    rcc_driver.enable_clock(rcc_api::Peripheral::QuadSpi);
    rcc_driver.leave_reset(rcc_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg);
    qspi.configure();

    // Board specific goo
    cfg_if::cfg_if! {
        if #[cfg(target_board = "gimlet-1")] {
            // Gimlet pin mapping
            // PF6 SP_QSPI1_IO3
            // PF7 SP_QSPI1_IO2
            // PF8 SP_QSPI1_IO0
            // PF9 SP_QSPI1_IO1
            // PF10 SP_QSPI1_CLK
            //
            // PG6 SP_QSPI1_CS
            //
            // PB2 SP_FLASH_TO_SP_RESET_L
            // PB1 SP_TO_SP3_FLASH_MUX_SELECT <-- low means us
            //
            gpio_driver.configure_alternate(
                gpio_api::Port::F.pin(6).and_pin(7).and_pin(10),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF9,
            ).unwrap();
            gpio_driver.configure_alternate(
                gpio_api::Port::F.pin(8).and_pin(9),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF10,
            ).unwrap();
            gpio_driver.configure_alternate(
                gpio_api::Port::G.pin(6),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF10,
            ).unwrap();

            // start reset and select off low
            gpio_driver.reset(gpio_api::Port::B.pin(1).and_pin(2)).unwrap();

            gpio_driver.configure_output(
                gpio_api::Port::B.pin(1).and_pin(2),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::High,
                gpio_api::Pull::None,
            ).unwrap();

            let reset_pin = gpio_api::Port::B.pin(2);
        } else if #[cfg(target_board = "gimletlet-2")] {
            // Gimletlet pin mapping
            // PF6 SP_QSPI1_IO3
            // PF7 SP_QSPI1_IO2
            // PF8 SP_QSPI1_IO0
            // PF9 SP_QSPI1_IO1
            // PF10 SP_QSPI1_CLK
            //
            // PG6 SP_QSPI1_CS
            //
            // TODO check these if I have a quadspimux board
            // PF4 SP_FLASH_TO_SP_RESET_L
            // PF5 SP_TO_SP3_FLASH_MUX_SELECT <-- low means us
            //
            gpio_driver.configure_alternate(
                gpio_api::Port::F.pin(6).and_pin(7).and_pin(10),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF9,
            ).unwrap();
            gpio_driver.configure_alternate(
                gpio_api::Port::F.pin(8).and_pin(9),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF10,
            ).unwrap();
            gpio_driver.configure_alternate(
                gpio_api::Port::G.pin(6),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF10,
            ).unwrap();

            // start reset and select off low
            gpio_driver.reset(gpio_api::Port::F.pin(4).and_pin(5)).unwrap();

            gpio_driver.configure_output(
                gpio_api::Port::F.pin(4).and_pin(5),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::High,
                gpio_api::Pull::None,
            ).unwrap();

            let reset_pin = gpio_api::Port::F.pin(4);
        } else if #[cfg(feature = "standalone")] {
            let reset_pin = gpio_api::Port::B.pin(2);
        } else {
            compile_error!("unsupported board");
        }
    }

    // Ensure hold time for reset in case we just restarted.
    // TODO look up actual hold time requirement
    hl::sleep_for(1);

    // Release reset and let it stabilize.
    gpio_driver.set(reset_pin).unwrap();
    hl::sleep_for(10);

    // Check the ID.
    {
        let mut idbuf = [0; 20];
        qspi.read_id(&mut idbuf);

        if idbuf[0] == 0x20 && idbuf[1] == 0xBA {
            // ok, I believe you
        } else {
            loop {
                // We are dead now.
                hl::sleep_for(1000);
            }
        }
    }

    let mut buffer = [0; 4];
    let mut block = [0; 256];

    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::ReadId => {
                let ((), caller) = msg.fixed().ok_or(HfError::Bad)?;

                let mut idbuf = [0; 20];
                qspi.read_id(&mut idbuf);

                caller.reply(idbuf);
                Ok::<_, HfError>(())
            }
            Op::ReadStatus => {
                let ((), caller) = msg.fixed().ok_or(HfError::Bad)?;

                let r = qspi.read_status();

                caller.reply(r);
                Ok::<_, HfError>(())
            }
            Op::BulkErase => {
                let ((), caller) = msg.fixed().ok_or(HfError::Bad)?;

                qspi.write_enable();
                let status = qspi.read_status();
                if status & 0b10 == 0 {
                    // oh oh
                    return Err(HfError::WriteEnableFailed);
                }
                qspi.bulk_erase();
                loop {
                    let status = qspi.read_status();
                    if status & 1 == 0 {
                        // ooh we're done
                        break;
                    }
                }

                caller.reply(());
                Ok::<_, HfError>(())
            }
            Op::PageProgram => {
                let (&addr, caller) = msg.fixed().ok_or(HfError::Bad)?;

                let borrow = caller.borrow(0);
                let info = borrow.info().ok_or(HfError::MissingLease)?;

                if !info.attributes.contains(LeaseAttributes::READ) {
                    return Err(HfError::BadLease);
                }
                if info.len > block.len() {
                    return Err(HfError::BadLease);
                }

                // Read the entire data block into our address space.
                borrow
                    .read_fully_at(0, &mut block[..info.len])
                    .ok_or(HfError::BadLease)?;

                // Now we can't fail.

                qspi.write_enable();
                let status = qspi.read_status();
                if status & 0b10 == 0 {
                    // oh oh
                    return Err(HfError::WriteEnableFailed);
                }

                qspi.page_program(addr, &block[..info.len]);
                loop {
                    let status = qspi.read_status();
                    if status & 1 == 0 {
                        // ooh we're done
                        break;
                    }
                }

                caller.reply(());
                Ok::<_, HfError>(())
            }
            Op::Read => {
                let (&addr, caller) = msg.fixed().ok_or(HfError::Bad)?;

                let borrow = caller.borrow(0);
                let info = borrow.info().ok_or(HfError::MissingLease)?;

                if !info.attributes.contains(LeaseAttributes::WRITE) {
                    return Err(HfError::BadLease);
                }
                if info.len > block.len() {
                    return Err(HfError::BadLease);
                }

                qspi.read_memory(addr, &mut block[..info.len]);

                // Throw away an error here since it means the caller's
                // wandered off
                borrow.write_fully_at(0, &block[..info.len]);

                caller.reply(());
                Ok::<_, HfError>(())
            }
            Op::SectorErase => {
                let (&addr, caller) = msg.fixed().ok_or(HfError::Bad)?;

                qspi.write_enable();
                let status = qspi.read_status();
                if status & 0b10 == 0 {
                    // oh oh
                    return Err(HfError::WriteEnableFailed);
                }

                qspi.sector_erase(addr);
                loop {
                    let status = qspi.read_status();
                    if status & 1 == 0 {
                        // ooh we're done
                        break;
                    }
                }

                caller.reply(());
                Ok::<_, HfError>(())
            }
        });
    }
}
