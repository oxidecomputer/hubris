//! Gimlet host flash server.
//!
//! This server is responsible for managing access to the host flash; it embeds
//! the QSPI flash driver.

#![no_std]
#![no_main]

use ringbuf::*;
use userlib::*;

use drv_stm32h7_gpio_api as gpio_api;
use drv_stm32h7_qspi::Qspi;
use drv_stm32h7_rcc_api as rcc_api;
use stm32h7::stm32h743 as device;

use drv_gimlet_hf_api::{HfError, InternalHfError, Operation};

task_slot!(RCC, rcc_driver);
task_slot!(GPIO, gpio_driver);

const QSPI_IRQ: u32 = 1;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    WriteEnableStatus(u8),
    Empty,
}

ringbuf!(Trace, 256, Trace::Empty);

#[export_name = "main"]
fn main() -> ! {
    let rcc_driver = rcc_api::Rcc::from(RCC.get_task_id());
    let gpio_driver = gpio_api::Gpio::from(GPIO.get_task_id());

    rcc_driver.enable_clock(rcc_api::Peripheral::QuadSpi);
    rcc_driver.leave_reset(rcc_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg, QSPI_IRQ);
    // Board specific goo
    cfg_if::cfg_if! {
        if #[cfg(target_board = "gimlet-1")] {
            qspi.configure(
                5, // 200MHz kernel / 5 = 40MHz clock
                25, // 2**25 = 32MiB = 256Mib
            );

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
            qspi.configure(
                5, // 200MHz kernel / 5 = 40MHz clock
                25, // 2**25 = 32MiB = 256Mib
            );
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
        } else if #[cfg(target_board = "nucleo-h743zi2")] {
            qspi.configure(
                50, // 200MHz kernel / 5 = 4MHz clock
                25, // 2**25 = 32MiB = 256Mib
            );
            // Nucleo-144 pin mapping
            // PB2 SP_QSPI1_CLK
            // PD11 SP_QSPI1_IO0
            // PD12 SP_QSPI1_IO1
            // PD13 SP_QSPI1_IO3
            // PE2 SP_QSPI1_IO2
            //
            // PG6 SP_QSPI1_CS
            //
            gpio_driver.configure_alternate(
                gpio_api::Port::B.pin(2),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF9,
            ).unwrap();
            gpio_driver.configure_alternate(
                gpio_api::Port::D.pin(11).and_pin(12).and_pin(13),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF9,
            ).unwrap();
            gpio_driver.configure_alternate(
                gpio_api::Port::E.pin(2),
                gpio_api::OutputType::PushPull,
                gpio_api::Speed::VeryHigh,
                gpio_api::Pull::None,
                gpio_api::Alternate::AF9,
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

        if idbuf[0] == 0x20 && matches!(idbuf[1], 0xBA | 0xBB) {
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
            Operation::ReadId => {
                let ((), caller) =
                    msg.fixed().ok_or(InternalHfError::BadMessage)?;

                let mut idbuf = [0; 20];
                qspi.read_id(&mut idbuf);

                caller.reply(idbuf);
                Ok::<_, InternalHfError>(())
            }
            Operation::ReadStatus => {
                let ((), caller) =
                    msg.fixed().ok_or(InternalHfError::BadMessage)?;

                caller.reply(qspi.read_status());
                Ok::<_, InternalHfError>(())
            }
            Operation::BulkErase => {
                let ((), caller) =
                    msg.fixed().ok_or(InternalHfError::BadMessage)?;

                set_and_check_write_enable(&qspi)?;
                qspi.bulk_erase();
                poll_for_write_complete(&qspi);

                caller.reply(());
                Ok::<_, InternalHfError>(())
            }
            Operation::PageProgram => {
                let (&addr, caller) =
                    msg.fixed().ok_or(InternalHfError::BadMessage)?;

                let borrow = caller.borrow(0);
                let info =
                    borrow.info().ok_or(InternalHfError::MissingLease)?;

                if !info.attributes.contains(LeaseAttributes::READ) {
                    return Err(InternalHfError::BadLease);
                }
                if info.len > block.len() {
                    return Err(InternalHfError::BadLease);
                }

                // Read the entire data block into our address space.
                borrow
                    .read_fully_at(0, &mut block[..info.len])
                    .ok_or(InternalHfError::BadLease)?;

                // Now we can't fail.

                set_and_check_write_enable(&qspi)?;
                qspi.page_program(addr, &block[..info.len]);
                poll_for_write_complete(&qspi);
                caller.reply(());
                Ok::<_, InternalHfError>(())
            }
            Operation::Read => {
                let (&addr, caller) =
                    msg.fixed().ok_or(InternalHfError::BadMessage)?;

                let borrow = caller.borrow(0);
                let info =
                    borrow.info().ok_or(InternalHfError::MissingLease)?;

                if !info.attributes.contains(LeaseAttributes::WRITE) {
                    return Err(InternalHfError::BadLease);
                }
                if info.len > block.len() {
                    return Err(InternalHfError::BadLease);
                }

                qspi.read_memory(addr, &mut block[..info.len]);

                // Throw away an error here since it means the caller's
                // wandered off
                borrow.write_fully_at(0, &block[..info.len]);

                caller.reply(());
                Ok::<_, InternalHfError>(())
            }
            Operation::SectorErase => {
                let (&addr, caller) =
                    msg.fixed().ok_or(InternalHfError::BadMessage)?;

                set_and_check_write_enable(&qspi)?;
                qspi.sector_erase(addr);
                poll_for_write_complete(&qspi);
                caller.reply(());
                Ok::<_, InternalHfError>(())
            }
        });
    }
}

fn set_and_check_write_enable(qspi: &Qspi) -> Result<(), HfError> {
    qspi.write_enable();
    let status = qspi.read_status();
    ringbuf_entry!(Trace::WriteEnableStatus(status));

    if status & 0b10 == 0 {
        // oh oh
        return Err(HfError::WriteEnableFailed.into());
    }
    Ok(())
}

fn poll_for_write_complete(qspi: &Qspi) {
    loop {
        let status = qspi.read_status();
        if status & 1 == 0 {
            // ooh we're done
            break;
        }
    }
}
