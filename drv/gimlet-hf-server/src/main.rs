// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

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
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};

// Note: h7b3 has QUADSPI but has not been used in this project.

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_gimlet_hf_api::HfError;

task_slot!(RCC, rcc_driver);
task_slot!(GPIO, gpio_driver);

const QSPI_IRQ: u32 = 1;

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

        } else if #[cfg(target_board = "gemini-bu-1")] {
            // PF4 HOST_ACCESS
            // PF5 RESET
            // PF6:AF9 IO3
            // PF7:AF9 IO2
            // PF8:AF10 IO0
            // PF9:AF10 IO1
            // PF10:AF9 CLK
            // PB6:AF10 CS
            qspi.configure(
                // Adjust this as needed for the SI and Logic Analyzer BW available
                200 / 25, // 200MHz kernel clock / $x MHz SPI clock = divisor
                25, // 2**25 = 32MiB = 256Mib
            );
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
                gpio_api::Port::B.pin(6),
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
            let reset_pin = gpio_api::Port::F.pin(5);
            let _host_access = gpio_api::Port::F.pin(4);

        } else if #[cfg(any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"))] {
            // Nucleo-h743zi2/h753zi pin mappings
            // These development boards are often wired by hand.
            // Although there are several choices for pin assignment,
            // the CN10 connector on the board has a marked "QSPI" block
            // of pins. Use those. Use two pull-up resistors and a
            // decoupling capacitor if needed.
            //
            // CNxx- Pin   MT25QL256xxx
            // pin   Fn    Pin           Signal   Notes
            // ----- ---   ------------, -------, ------
            // 10-07 PF4,  3,            RESET#,  10K ohm to Vcc
            // 10-09 PF5,  ---           nc,
            // 10-11 PF6,  ---           nc,
            // 10-13 PG6,  7,            CS#,     10K ohm to Vcc
            // 10-15 PB2,  16,           CLK,
            // 10-17 GND,  10,           GND,
            // 10-19 PD13, 1,            IO3,
            // 10-21 PD12, 8,            IO1,
            // 10-23 PD11, 15,           IO0,
            // 10-25 PE2,  9,            IO2,
            // 10-27 GND,  ---           nc,
            // 10-29 PA0,  ---           nc,
            // 10-31 PB0,  ---           nc,
            // 10-33 PE0,  ---           nc,
            //
            // 08-07 3V3,  2,            Vcc,     100nF to GND
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

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        qspi,
        block: [0; 256],
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    qspi: Qspi,
    block: [u8; 256],
}

impl idl::InOrderHostFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 20], RequestError<HfError>> {
        let mut idbuf = [0; 20];
        self.qspi.read_id(&mut idbuf);
        Ok(idbuf)
    }

    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<HfError>> {
        Ok(self.qspi.read_status())
    }

    fn bulk_erase(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<HfError>> {
        set_and_check_write_enable(&self.qspi)?;
        self.qspi.bulk_erase();
        poll_for_write_complete(&self.qspi);
        Ok(())
    }

    fn page_program(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        data: LenLimit<Leased<R, [u8]>, 256>,
    ) -> Result<(), RequestError<HfError>> {
        // Read the entire data block into our address space.
        data.read_range(0..data.len(), &mut self.block[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        // Now we can't fail.

        set_and_check_write_enable(&self.qspi)?;
        self.qspi.page_program(addr, &self.block[..data.len()]);
        poll_for_write_complete(&self.qspi);
        Ok(())
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, 256>,
    ) -> Result<(), RequestError<HfError>> {
        self.qspi.read_memory(addr, &mut self.block[..dest.len()]);

        dest.write_range(0..dest.len(), &self.block[..dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(())
    }

    fn sector_erase(
        &mut self,
        _: &RecvMessage,
        addr: u32,
    ) -> Result<(), RequestError<HfError>> {
        set_and_check_write_enable(&self.qspi)?;
        self.qspi.sector_erase(addr);
        poll_for_write_complete(&self.qspi);
        Ok(())
    }
}

fn set_and_check_write_enable(
    qspi: &Qspi,
) -> Result<(), RequestError<HfError>> {
    qspi.write_enable();
    let status = qspi.read_status();

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

mod idl {
    use super::HfError;

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
