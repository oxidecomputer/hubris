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

use drv_stm32h7_qspi::Qspi;
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

// hash_api is optional, but idl files don't have support for optional APIs.
// So, always include and return a "not implemented" error if the
// feature is absent.
#[cfg(feature = "hash")]
use drv_hash_api as hash_api;
use drv_hash_api::SHA256_SZ;

use drv_gimlet_hf_api::{HfError, HfMuxState};

task_slot!(SYS, sys);
#[cfg(feature = "hash")]
task_slot!(HASH, hash_driver);

const QSPI_IRQ: u32 = 1;

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.enable_clock(sys_api::Peripheral::QuadSpi);
    sys.leave_reset(sys_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg, QSPI_IRQ);
    // Board specific goo
    cfg_if::cfg_if! {
        if #[cfg(any(target_board = "gimlet-a", target_board = "gimlet-b"))] {
            let clock = 5; // 200MHz kernel / 5 = 40MHz clock
            qspi.configure(
                clock,
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
            sys.gpio_configure_alternate(
                sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
                sys_api::OutputType::PushPull,
                sys_api::Speed::VeryHigh,
                sys_api::Pull::None,
                sys_api::Alternate::AF9,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::F.pin(8).and_pin(9),
                sys_api::OutputType::PushPull,
                sys_api::Speed::VeryHigh,
                sys_api::Pull::None,
                sys_api::Alternate::AF10,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::G.pin(6),
                sys_api::OutputType::PushPull,
                sys_api::Speed::VeryHigh,
                sys_api::Pull::None,
                sys_api::Alternate::AF10,
            ).unwrap();

            // start reset and select off low
            sys.gpio_reset(sys_api::Port::B.pin(1).and_pin(2)).unwrap();

            sys.gpio_configure_output(
                sys_api::Port::B.pin(1).and_pin(2),
                sys_api::OutputType::PushPull,
                sys_api::Speed::High,
                sys_api::Pull::None,
            ).unwrap();

            let select_pin = sys_api::Port::B.pin(1);
            let reset_pin = sys_api::Port::B.pin(2);
        } else if #[cfg(target_board = "gimletlet-2")] {
            let clock = 5; // 200MHz kernel / 5 = 40MHz clock
            qspi.configure(
                clock,
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
            sys.gpio_configure_alternate(
                sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
                sys_api::OutputType::PushPull,
                sys_api::Speed::VeryHigh,
                sys_api::Pull::None,
                sys_api::Alternate::AF9,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::F.pin(8).and_pin(9),
                sys_api::OutputType::PushPull,
                sys_api::Speed::VeryHigh,
                sys_api::Pull::None,
                sys_api::Alternate::AF10,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::G.pin(6),
                sys_api::OutputType::PushPull,
                sys_api::Speed::VeryHigh,
                sys_api::Pull::None,
                sys_api::Alternate::AF10,
            ).unwrap();

            // start reset and select off low
            sys.gpio_reset(sys_api::Port::F.pin(4).and_pin(5)).unwrap();

            sys.gpio_configure_output(
                sys_api::Port::F.pin(4).and_pin(5),
                sys_api::OutputType::PushPull,
                sys_api::Speed::High,
                sys_api::Pull::None,
            ).unwrap();

            let select_pin = sys_api::Port::F.pin(5);
            let reset_pin = sys_api::Port::F.pin(4);
        } else if #[cfg(target_board = "gemini-bu-1")] {
            // PF4 HOST_ACCESS
            // PF5 RESET
            // PF6:AF9 IO3
            // PF7:AF9 IO2
            // PF8:AF10 IO0
            // PF9:AF10 IO1
            // PF10:AF9 CLK
            // PB6:AF10 CS
            let clock = 200 / 25; // 200MHz kernel clock / $x MHz SPI clock = divisor
            qspi.configure(
                clock,
                25, // 2**25 = 32MiB = 256Mib
            );
            sys.gpio_configure_alternate(
                sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
                sys_api::Alternate::AF9,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::F.pin(8).and_pin(9),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
                sys_api::Alternate::AF10,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::B.pin(6),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
                sys_api::Alternate::AF10,
            ).unwrap();

            // start reset and select off low
            sys.gpio_reset(sys_api::Port::F.pin(4).and_pin(5)).unwrap();

            sys.gpio_configure_output(
                sys_api::Port::F.pin(4).and_pin(5),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
            ).unwrap();
            let select_pin = sys_api::Port::F.pin(4);
            let reset_pin = sys_api::Port::F.pin(5);
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
            let clock = 8; // 200MHz kernel / 8 = 25MHz clock
            qspi.configure(
                clock,
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
            sys.gpio_configure_alternate(
                sys_api::Port::B.pin(2),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
                sys_api::Alternate::AF9,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::D.pin(11).and_pin(12).and_pin(13),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
                sys_api::Alternate::AF9,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::E.pin(2),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
                sys_api::Alternate::AF9,
            ).unwrap();
            sys.gpio_configure_alternate(
                sys_api::Port::G.pin(6),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
                sys_api::Alternate::AF10,
            ).unwrap();

            // start reset and select off low
            sys.gpio_reset(sys_api::Port::F.pin(4).and_pin(5)).unwrap();

            sys.gpio_configure_output(
                sys_api::Port::F.pin(4).and_pin(5),
                sys_api::OutputType::PushPull,
                sys_api::Speed::Low,
                sys_api::Pull::None,
            ).unwrap();

            let select_pin = sys_api::Port::F.pin(5);
            let reset_pin = sys_api::Port::F.pin(4);
        } else {
            compile_error!("unsupported board");
        }
    }

    // TODO: The best clock frequency to use can vary based on the flash
    // part, the command used, and signal integrity limits of the board.

    // Ensure hold time for reset in case we just restarted.
    // TODO look up actual hold time requirement
    hl::sleep_for(1);

    // Release reset and let it stabilize.
    sys.gpio_set(reset_pin).unwrap();
    hl::sleep_for(10);

    // Check the ID.
    // TODO: If different flash parts are used on the same board name,
    // then hard-coding commands, capacity, and clocks will get us into
    // trouble. Someday we will need more flexability here.
    let capacity = {
        let mut idbuf = [0; 20];
        qspi.read_id(&mut idbuf);

        match idbuf[0] {
            0x00 => None, // Invalid
            0xef => {
                // Winbond
                if idbuf[1] != 0x40 {
                    None
                } else {
                    Some(idbuf[2])
                }
            }
            0x20 => {
                if !matches!(idbuf[1], 0xBA | 0xBB) {
                    // 1.8v or 3.3v
                    None
                } else {
                    // TODO: Stash, or read on demand, Micron Unique ID for measurement?
                    Some(idbuf[2])
                }
            }
            _ => None, // Unknown
        }
    };

    if capacity.is_none() {
        loop {
            // We are dead now.
            hl::sleep_for(1000);
        }
    }
    let capacity = capacity.unwrap();
    qspi.configure(clock, capacity);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        qspi,
        block: [0; 256],
        mux_state: HfMuxState::SP,
        select_pin,
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    qspi: Qspi,
    block: [u8; 256],
    mux_state: HfMuxState,
    select_pin: drv_stm32xx_sys_api::PinSet,
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

    fn get_mux(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfMuxState, RequestError<HfError>> {
        Ok(self.mux_state)
    }

    fn set_mux(
        &mut self,
        _: &RecvMessage,
        state: HfMuxState,
    ) -> Result<(), RequestError<HfError>> {
        let sys = sys_api::Sys::from(SYS.get_task_id());

        let rv = match state {
            HfMuxState::SP => sys.gpio_reset(self.select_pin),
            HfMuxState::HostCPU => sys.gpio_set(self.select_pin),
        };

        match rv {
            Err(_) => Err(HfError::MuxFailed.into()),
            Ok(_) => {
                self.mux_state = state;
                Ok(())
            }
        }
    }

    cfg_if::cfg_if! {
        if #[cfg(feature = "hash")] {
            fn hash(
                &mut self,
                _: &RecvMessage,
                addr: u32,
                len: u32,
            ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
                let hash_driver = hash_api::Hash::from(HASH.get_task_id());
                if let Err(_) = hash_driver.init_sha256() {
                    return Err(HfError::HashError.into());
                }
                let begin = addr as usize;
                // TODO: Begin may be an address beyond physical end of
                // flash part and may wrap around.
                let end = match begin.checked_add(len as usize) {
                    Some(end) => {
                        // Check end > maximum 4-byte address.
                        // TODO: End may be beyond physical end of flash part.
                        //       Use that limit rather than maximum 4-byte address.
                        if end > u32::MAX as usize {
                            return Err(HfError::HashBadRange.into());
                        } else {
                            end
                        }
                    },
                    None => {
                        return Err(HfError::HashBadRange.into());
                    },
                };
                // If we knew the flash part size, we'd check against those limits.
                for addr in (begin..end).step_by(self.block.len()) {
                    let size = if self.block.len() < (end - addr) {
                        self.block.len()
                    } else {
                        end - addr
                    };
                    self.qspi.read_memory(addr as u32, &mut self.block[..size]);
                    if let Err(_) = hash_driver.update(
                        size as u32, &self.block[..size]) {
                        return Err(HfError::HashError.into());
                    }
                }
                match hash_driver.finalize_sha256() {
                    Ok(sum) => Ok(sum),
                    Err(_) => Err(HfError::HashError.into()),   // XXX losing info
                }
            }
        } else {
            fn hash(
                &mut self,
                _: &RecvMessage,
                _addr: u32,
                _len: u32,
            ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
                Err(HfError::HashNotConfigured.into())
            }
        }
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
    use super::{HfError, HfMuxState};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
