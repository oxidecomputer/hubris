// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Gimlet host flash server.
//!
//! This server is responsible for managing access to the host flash; it embeds
//! the QSPI flash driver.

#![no_std]
#![no_main]

#[cfg_attr(target_board = "gimlet-b", path = "bsp/gimlet_bc.rs")]
#[cfg_attr(target_board = "gimlet-c", path = "bsp/gimlet_bc.rs")]
#[cfg_attr(target_board = "gemini-bu-1", path = "bsp/gemini_bu_1.rs")]
#[cfg_attr(target_board = "gimletlet-2", path = "bsp/gimletlet_2.rs")]
#[cfg_attr(
    any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"),
    path = "bsp/nucleo_h7x.rs"
)]
mod bsp;

use userlib::*;

use drv_gimlet_hf_api::SECTOR_SIZE_BYTES;
use drv_stm32h7_qspi::Qspi;
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};
use zerocopy::{AsBytes, FromBytes};

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

use drv_gimlet_hf_api::{
    HfDevSelect, HfError, HfMuxState, HfPersistentData, HfProtectMode,
    PAGE_SIZE_BYTES,
};

task_slot!(SYS, sys);
#[cfg(feature = "hash")]
task_slot!(HASH, hash_driver);

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
            sys.gpio_reset(p);

            sys.gpio_configure_output(
                p,
                sys_api::OutputType::PushPull,
                sys_api::Speed::High,
                sys_api::Pull::None,
            );
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.enable_clock(sys_api::Peripheral::QuadSpi);
    sys.leave_reset(sys_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg, notifications::QSPI_IRQ_MASK);

    // Build a pin struct using a board-specific init function
    let cfg = bsp::init(&qspi, &sys);
    cfg.init(&sys);

    // TODO: The best clock frequency to use can vary based on the flash
    // part, the command used, and signal integrity limits of the board.

    // Ensure hold time for reset in case we just restarted.
    // TODO look up actual hold time requirement
    hl::sleep_for(1);

    // Release reset and let it stabilize.
    sys.gpio_set(cfg.reset);
    hl::sleep_for(10);

    // Check the ID.
    // TODO: If different flash parts are used on the same board name,
    // then hard-coding commands, capacity, and clocks will get us into
    // trouble. Someday we will need more flexability here.
    let log2_capacity = {
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

    let Some(log2_capacity) = log2_capacity else {
        loop {
            // We are dead now.
            hl::sleep_for(1000);
        }
    };
    qspi.configure(cfg.clock, log2_capacity);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        qspi,
        block: [0; 256],
        capacity: 1 << log2_capacity,
        mux_state: HfMuxState::SP,
        dev_state: HfDevSelect::Flash0,
        mux_select_pin: cfg.sp_host_mux_select,
        dev_select_pin: cfg.flash_dev_select,
    };

    server.ensure_persistent_data_is_redundant().unwrap(); // TODO: log this?

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

/// Represents persistent data that is both stored on the host flash and used to
/// configure host boot.
///
/// We reserve sector 0 (i.e. the lowest 64 KiB) of both host flash ICs for
/// Hubris data.  On both host flashes, we tile sector 0 with this 40-byte
/// struct, placed at 128-byte intervals (in case it needs to grow in the
/// future).
///
/// The current value of persistent data is the instance of `RawPersistentData`
/// with a valid checksum and the highest `epoch` across both flash ICs.
///
/// When writing new data, we increment the epoch and write to both ICs, one by
/// one.  This ensures robustness in case of power loss.
#[derive(Copy, Clone, AsBytes, FromBytes)]
#[repr(C)]
struct RawPersistentData {
    /// Reserved field, because this is placed at address 0, which PSP firmware
    /// may look at under certain circumstances.
    amd_reserved_must_be_all_ones: u64,

    /// Must always be `HF_PERSISTENT_DATA_MAGIC`.
    oxide_magic: u32,

    /// Must always be `HF_PERSISTENT_DATA_HEADER_VERSION` (for now)
    header_version: u32,

    /// Monotonically increasing counter
    epoch: u64,

    /// Equivalent to `host_sp_messages::HostStartupOptions`
    host_startup_options: u64,

    /// Either 0 or 1; directly translatable to `gimlet_hf_api::HfDevSelect`
    dev_select: u32,

    /// CRC-32 over the rest of the data using the iSCSI polynomial
    checksum: u32,
}

impl RawPersistentData {
    fn new(data: HfPersistentData, epoch: u64) -> Self {
        let mut out = Self {
            amd_reserved_must_be_all_ones: u64::MAX,
            oxide_magic: HF_PERSISTENT_DATA_MAGIC,
            header_version: HF_PERSISTENT_DATA_HEADER_VERSION,
            epoch,
            host_startup_options: data.startup_options,
            dev_select: data.dev_select as u32,
            checksum: 0,
        };
        out.checksum = out.expected_checksum();
        assert!(out.is_valid());
        out
    }

    fn expected_checksum(&self) -> u32 {
        let c = crc::Crc::<u32>::new(&crc::CRC_32_ISCSI);
        let mut c = c.digest();
        // We do a CRC32 of everything except the checksum, which is positioned
        // at the end of the struct and is a `u32`
        let size = core::mem::size_of::<RawPersistentData>()
            - core::mem::size_of::<u32>();
        c.update(&self.as_bytes()[..size]);
        c.finalize()
    }

    fn is_valid(&self) -> bool {
        self.amd_reserved_must_be_all_ones == u64::MAX
            && self.oxide_magic == HF_PERSISTENT_DATA_MAGIC
            && self.header_version == HF_PERSISTENT_DATA_HEADER_VERSION
            && self.dev_select <= 1
            && self.checksum == self.expected_checksum()
    }
}

const HF_PERSISTENT_DATA_MAGIC: u32 = 0x1dea_bcde;
const HF_PERSISTENT_DATA_STRIDE: usize = 128;
const HF_PERSISTENT_DATA_HEADER_VERSION: u32 = 1;

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl {
    qspi: Qspi,
    block: [u8; 256],
    capacity: usize,

    /// Selects between the SP and SP3 talking to the QSPI flash
    mux_state: HfMuxState,
    mux_select_pin: sys_api::PinSet,

    /// Selects between QSPI flash chips 1 and 2 (if present)
    dev_state: HfDevSelect,
    dev_select_pin: Option<sys_api::PinSet>,
}

impl ServerImpl {
    ///
    /// For operations to host flash from the SP, we need to have the flash
    /// muxed to the SP; to assure that we fail cleanly (and do not attempt
    /// to interact with a device that in fact cannot see us), we call this
    /// convenience routine to fail explicitly should a host flash operation be
    /// attempted while in the wrong mux state.
    ///
    fn check_muxed_to_sp(&self) -> Result<(), HfError> {
        match self.mux_state {
            HfMuxState::SP => Ok(()),
            HfMuxState::HostCPU => Err(HfError::NotMuxedToSP),
        }
    }

    fn page_program_raw(
        qspi: &Qspi,
        addr: u32,
        data: &[u8],
    ) -> Result<(), HfError> {
        set_and_check_write_enable(qspi)?;
        qspi.page_program(addr, data);
        poll_for_write_complete(qspi, None);
        Ok(())
    }

    fn set_dev(&mut self, state: HfDevSelect) -> Result<(), HfError> {
        // Return early if the dev select pin is missing
        let dev_select_pin = self.dev_select_pin.ok_or(HfError::NoDevSelect)?;

        self.check_muxed_to_sp()?;

        let sys = sys_api::Sys::from(SYS.get_task_id());
        match state {
            HfDevSelect::Flash0 => sys.gpio_reset(dev_select_pin),
            HfDevSelect::Flash1 => sys.gpio_set(dev_select_pin),
        }

        self.dev_state = state;
        Ok(())
    }

    fn get_raw_persistent_data(
        &mut self,
    ) -> Result<RawPersistentData, HfError> {
        self.check_muxed_to_sp()?;
        let best = if self.dev_select_pin.is_some() {
            let prev_slot = self.dev_state;

            // At this point, all of the ways that `set_dev` could fail have
            // been checked: we already called both `check_muxed_to_sp` and that
            // `dev_select_pin` is `Some(...)`, so we can unwrap returns from
            // `self.set_dev(...)`.

            // Look at the inactive slot first
            self.set_dev(!prev_slot).unwrap();
            let (a, _) = self.persistent_data_scan();

            // Then switch back to our current slot, so that the resulting state
            // is unchanged.
            self.set_dev(prev_slot).unwrap();
            let (b, _) = self.persistent_data_scan();

            match (a, b) {
                (Some(a), Some(b)) => {
                    if a.epoch > b.epoch {
                        Some(a)
                    } else {
                        Some(b)
                    }
                }
                (Some(_), None) => a,
                (None, Some(_)) => b,
                (None, None) => None,
            }
        } else {
            // The single-chip case is easy:
            self.persistent_data_scan().0
        };

        best.ok_or(HfError::NoPersistentData)
    }

    /// Scans the currently active flash IC for persistent data
    ///
    /// Returns a tuple containing
    /// - newest persistent data record
    /// - address of first empty slot
    fn persistent_data_scan(&self) -> (Option<RawPersistentData>, Option<u32>) {
        let mut best: Option<RawPersistentData> = None;
        let mut empty_slot: Option<u32> = None;
        for i in 0..SECTOR_SIZE_BYTES / HF_PERSISTENT_DATA_STRIDE {
            let addr = (i * HF_PERSISTENT_DATA_STRIDE) as u32;
            let mut data = RawPersistentData::new_zeroed();
            self.qspi.read_memory(addr, data.as_bytes_mut());
            if data.is_valid()
                && best.map(|b| b.epoch < data.epoch).unwrap_or(true)
            {
                best = Some(data)
            }
            if empty_slot.is_none()
                && data.as_bytes().iter().all(|b| *b == 0xFF)
            {
                empty_slot = Some(addr);
            }
        }
        (best, empty_slot)
    }

    /// Erases the sector containing the given address
    ///
    /// If this is sector 0, requires `protect` to be
    /// `HfProtectMode::AllowModificationsToSector0` (otherwise this function
    /// will return an error).
    fn sector_erase(
        &mut self,
        addr: u32,
        protect: HfProtectMode,
    ) -> Result<(), HfError> {
        if addr as usize / SECTOR_SIZE_BYTES == 0
            && !matches!(protect, HfProtectMode::AllowModificationsToSector0)
        {
            return Err(HfError::Sector0IsReserved.into());
        }
        self.check_muxed_to_sp()?;
        set_and_check_write_enable(&self.qspi)?;
        self.qspi.sector_erase(addr);
        poll_for_write_complete(&self.qspi, Some(1));
        Ok(())
    }

    /// Writes raw persistent data to the given address on the
    /// currently-selected flash IC.
    ///
    /// If `addr` is `None`, then we're out of available space; erase all of
    /// sector 0 and write to address 0 upon success.
    fn write_raw_persistent_data_to_addr(
        &mut self,
        addr: Option<u32>,
        raw_data: &RawPersistentData,
    ) -> Result<(), HfError> {
        let addr = match addr {
            Some(a) => a,
            None => {
                self.sector_erase(
                    0,
                    HfProtectMode::AllowModificationsToSector0,
                )?;
                0
            }
        };
        Self::page_program_raw(&self.qspi, addr, raw_data.as_bytes())
    }

    /// Checks that the persistent data is consistent between the two flash ICs.
    fn ensure_persistent_data_is_redundant(&mut self) -> Result<(), HfError> {
        // This should only be called on startup, at which point we're always
        // muxed to the SP.
        self.check_muxed_to_sp().unwrap();

        // We can't have redundant data if we've only got a single flash
        if self.dev_select_pin.is_none() {
            return Ok(());
        }

        // Load the current state of persistent data from flash
        let prev_slot = self.dev_state;
        self.set_dev(!prev_slot).unwrap();
        let (a_data, a_next) = self.persistent_data_scan();

        self.set_dev(prev_slot).unwrap();
        let (b_data, b_next) = self.persistent_data_scan();

        match (a_data, b_data) {
            (Some(a), Some(b)) => match a.epoch.cmp(&b.epoch) {
                core::cmp::Ordering::Less => {
                    self.set_dev(!prev_slot).unwrap();
                    let out =
                        self.write_raw_persistent_data_to_addr(a_next, &b);
                    self.set_dev(prev_slot).unwrap();
                    out
                }
                core::cmp::Ordering::Greater => {
                    self.write_raw_persistent_data_to_addr(b_next, &a)
                }
                core::cmp::Ordering::Equal => {
                    // Redundant data is consistent
                    // TODO: should we have a special case if they don't agree?
                    Ok(())
                }
            },
            (Some(a), None) => {
                self.write_raw_persistent_data_to_addr(b_next, &a)
            }
            (None, Some(b)) => {
                self.set_dev(!prev_slot).unwrap();
                let out = self.write_raw_persistent_data_to_addr(a_next, &b);
                self.set_dev(prev_slot).unwrap();
                out
            }
            (None, None) => {
                // No persistent data recorded; nothing to do here
                Ok(())
            }
        }
    }
}

impl idl::InOrderHostFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 20], RequestError<HfError>> {
        self.check_muxed_to_sp()?;

        let mut idbuf = [0; 20];
        self.qspi.read_id(&mut idbuf);
        Ok(idbuf)
    }

    fn capacity(
        &mut self,
        _: &RecvMessage,
    ) -> Result<usize, RequestError<HfError>> {
        Ok(self.capacity)
    }

    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<HfError>> {
        self.check_muxed_to_sp()?;
        Ok(self.qspi.read_status())
    }

    fn bulk_erase(
        &mut self,
        _: &RecvMessage,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        if !matches!(protect, HfProtectMode::AllowModificationsToSector0) {
            return Err(HfError::Sector0IsReserved.into());
        }
        self.check_muxed_to_sp()?;
        set_and_check_write_enable(&self.qspi)?;
        self.qspi.bulk_erase();
        poll_for_write_complete(&self.qspi, Some(100));
        Ok(())
    }

    fn page_program(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        protect: HfProtectMode,
        data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        if addr as usize / SECTOR_SIZE_BYTES == 0
            && !matches!(protect, HfProtectMode::AllowModificationsToSector0)
        {
            return Err(HfError::Sector0IsReserved.into());
        }
        self.check_muxed_to_sp()?;
        // Read the entire data block into our address space.
        data.read_range(0..data.len(), &mut self.block[..data.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        // Now we can't fail. (TODO is this comment outdated?)
        Self::page_program_raw(&self.qspi, addr, &self.block[..data.len()])?;
        Ok(())
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<HfError>> {
        self.check_muxed_to_sp()?;
        self.qspi.read_memory(addr, &mut self.block[..dest.len()]);

        dest.write_range(0..dest.len(), &self.block[..dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(())
    }

    fn sector_erase(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        protect: HfProtectMode,
    ) -> Result<(), RequestError<HfError>> {
        self.sector_erase(addr, protect).map_err(RequestError::from)
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

        match state {
            HfMuxState::SP => sys.gpio_reset(self.mux_select_pin),
            HfMuxState::HostCPU => sys.gpio_set(self.mux_select_pin),
        }

        self.mux_state = state;
        Ok(())
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
        self.set_dev(state).map_err(RequestError::from)
    }

    #[cfg(feature = "hash")]
    fn hash(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        len: u32,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        self.check_muxed_to_sp()?;
        let hash_driver = hash_api::Hash::from(HASH.get_task_id());
        if hash_driver.init_sha256().is_err() {
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
            }
            None => {
                return Err(HfError::HashBadRange.into());
            }
        };
        // If we knew the flash part size, we'd check against those limits.
        for addr in (begin..end).step_by(self.block.len()) {
            let size = if self.block.len() < (end - addr) {
                self.block.len()
            } else {
                end - addr
            };
            self.qspi.read_memory(addr as u32, &mut self.block[..size]);
            if hash_driver
                .update(size as u32, &self.block[..size])
                .is_err()
            {
                return Err(HfError::HashError.into());
            }
        }
        match hash_driver.finalize_sha256() {
            Ok(sum) => Ok(sum),
            Err(_) => Err(HfError::HashError.into()), // XXX losing info
        }
    }

    #[cfg(not(feature = "hash"))]
    fn hash(
        &mut self,
        _: &RecvMessage,
        _addr: u32,
        _len: u32,
    ) -> Result<[u8; SHA256_SZ], RequestError<HfError>> {
        Err(HfError::HashNotConfigured.into())
    }

    fn get_persistent_data(
        &mut self,
        _: &RecvMessage,
    ) -> Result<HfPersistentData, RequestError<HfError>> {
        let out = self.get_raw_persistent_data()?;
        Ok(HfPersistentData {
            startup_options: out.host_startup_options,
            dev_select: HfDevSelect::from_u8(out.dev_select as u8).unwrap(),
        })
    }

    /// Writes the given persistent data to host flash
    fn write_persistent_data(
        &mut self,
        _: &RecvMessage,
        startup_options: u64,
        dev_select: HfDevSelect,
    ) -> Result<(), RequestError<HfError>> {
        let data = HfPersistentData {
            startup_options,
            dev_select,
        };
        self.check_muxed_to_sp()?;
        if self.dev_select_pin.is_some() {
            let prev_slot = self.dev_state;

            self.set_dev(!prev_slot).unwrap();
            let (a_data, a_next) = self.persistent_data_scan();

            self.set_dev(prev_slot).unwrap();
            let (b_data, b_next) = self.persistent_data_scan();

            let prev_epoch = match (a_data, b_data) {
                (Some(a), Some(b)) => a.epoch.max(b.epoch),
                (Some(a), None) => a.epoch,
                (None, Some(b)) => b.epoch,
                (None, None) => 0,
            };
            let epoch = prev_epoch + 1;
            let raw = RawPersistentData::new(data, epoch);

            // Write the persistent data to the currently inactive flash.
            //
            // At this point, all of the ways that `set_dev` could fail have
            // been checked: we already called both `check_muxed_to_sp` and that
            // `dev_select_pin` is `Some(...)`, so we can unwrap returns from
            // `self.set_dev(...)`.
            self.set_dev(!prev_slot).unwrap();
            let out_a = self.write_raw_persistent_data_to_addr(a_next, &raw);

            // Write the persistent data to the currently active flash
            self.set_dev(prev_slot).unwrap();

            // Now that we've restored the current active flash, check whether
            // we should propagate errors.
            out_a?;

            self.write_raw_persistent_data_to_addr(b_next, &raw)?;
        } else {
            // Single-flash case is less complicated, because we don't have to
            // track and maintain the active device.
            let (slot, next) = self.persistent_data_scan();
            let prev_epoch = match slot {
                Some(a) => a.epoch,
                None => 0,
            };
            let epoch = prev_epoch + 1;
            let raw = RawPersistentData::new(data, epoch);
            self.write_raw_persistent_data_to_addr(next, &raw)?;
        }
        Ok(())
    }
}

fn set_and_check_write_enable(qspi: &Qspi) -> Result<(), HfError> {
    qspi.write_enable();
    let status = qspi.read_status();

    if status & 0b10 == 0 {
        // oh oh
        return Err(HfError::WriteEnableFailed);
    }
    Ok(())
}

fn poll_for_write_complete(qspi: &Qspi, sleep_between_polls: Option<u64>) {
    loop {
        let status = qspi.read_status();
        if status & 1 == 0 {
            // ooh we're done
            break;
        }
        if let Some(ticks) = sleep_between_polls {
            hl::sleep_for(ticks);
        }
    }
}

mod idl {
    use super::{
        HfDevSelect, HfError, HfMuxState, HfPersistentData, HfProtectMode,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
