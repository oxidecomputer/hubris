// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Gimlet host flash server.
//!
//! This server is responsible for managing access to the host flash; it embeds
//! the QSPI flash driver.

#![no_std]
#![no_main]

mod bsp;

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

use drv_gimlet_hf_api::{HfDevSelect, HfError, HfMuxState};

task_slot!(SYS, sys);
#[cfg(feature = "hash")]
task_slot!(HASH, hash_driver);

const QSPI_IRQ: u32 = 1;

struct Config {
    pub sp_host_mux_select: sys_api::PinSet,
    pub reset: sys_api::PinSet,
    pub flash_dev_select: Option<sys_api::PinSet>,
    pub clock: u8,
}

impl Config {
    fn init(&self, sys: &sys_api::Sys) {
        // start with reset, mux select, and dev select all low
        for &p in [self.reset, self.sp_host_mux_select]
            .iter()
            .chain(self.flash_dev_select.as_ref().into_iter())
        {
            sys.gpio_reset(p).unwrap();

            sys.gpio_configure_output(
                p,
                sys_api::OutputType::PushPull,
                sys_api::Speed::High,
                sys_api::Pull::None,
            )
            .unwrap();
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.enable_clock(sys_api::Peripheral::QuadSpi);
    sys.leave_reset(sys_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg, QSPI_IRQ);

    // Build a pin struct using a board-specific init function
    let cfg = bsp::init(&qspi, &sys);
    cfg.init(&sys);

    // TODO: The best clock frequency to use can vary based on the flash
    // part, the command used, and signal integrity limits of the board.

    // Ensure hold time for reset in case we just restarted.
    // TODO look up actual hold time requirement
    hl::sleep_for(1);

    // Release reset and let it stabilize.
    sys.gpio_set(cfg.reset).unwrap();
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
    qspi.configure(cfg.clock, capacity);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        qspi,
        block: [0; 256],
        mux_state: HfMuxState::SP,
        dev_state: HfDevSelect::Flash0,
        mux_select_pin: cfg.sp_host_mux_select,
        dev_select_pin: cfg.flash_dev_select,
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    qspi: Qspi,
    block: [u8; 256],

    /// Selects between the SP and SP3 talking to the QSPI flash
    mux_state: HfMuxState,
    mux_select_pin: sys_api::PinSet,

    /// Selects between QSPI flash chips 1 and 2 (if present)
    dev_state: HfDevSelect,
    dev_select_pin: Option<sys_api::PinSet>,
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
            HfMuxState::SP => sys.gpio_reset(self.mux_select_pin),
            HfMuxState::HostCPU => sys.gpio_set(self.mux_select_pin),
        };

        match rv {
            Err(_) => Err(HfError::MuxFailed.into()),
            Ok(_) => {
                self.mux_state = state;
                Ok(())
            }
        }
    }

    fn get_dev(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfDevSelect, RequestError<HfError>> {
        Ok(self.dev_state)
    }

    fn set_dev(
        &mut self,
        _: &RecvMessage,
        state: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        // Return early if the dev select pin is missing
        let dev_select_pin = self.dev_select_pin.ok_or(HfError::NoDevSelect)?;

        let sys = sys_api::Sys::from(SYS.get_task_id());
        let rv = match state {
            HfDevSelect::Flash0 => sys.gpio_reset(dev_select_pin),
            HfDevSelect::Flash1 => sys.gpio_set(dev_select_pin),
        };

        match rv {
            Err(_) => Err(HfError::DevSelectFailed.into()),
            Ok(_) => {
                self.dev_state = state;
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
    use super::{HfDevSelect, HfError, HfMuxState};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
