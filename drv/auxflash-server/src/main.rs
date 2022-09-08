// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use drv_auxflash_api::{AuxFlashChecksum, AuxFlashError, AuxFlashId};
use idol_runtime::{ClientError, Leased, LenLimit, RequestError, R, W};
use sha3::{Digest, Sha3_256};
use tlvc::{TlvcRead, TlvcReadError, TlvcReader};
use userlib::*;

// XXX hard-coded for Sidecar
const SLOT_COUNT: u32 = 16;

// Generic across all machines
const SLOT_SIZE: u32 = 1 << 20;

#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_stm32h7_qspi::Qspi;
use drv_stm32xx_sys_api as sys_api;

task_slot!(SYS, sys);
const QSPI_IRQ: u32 = 1;

////////////////////////////////////////////////////////////////////////////////

/// Simple handle which holds a `&Qspi` and allows us to implement `TlvcRead`
#[derive(Copy, Clone)]
struct SlotReader<'a> {
    qspi: &'a Qspi,
    base: u32,
}

impl<'a> TlvcRead for SlotReader<'a> {
    fn extent(&self) -> Result<u64, TlvcReadError> {
        // Hard-coded slot size of 1MiB
        Ok(SLOT_SIZE as u64)
    }
    fn read_exact(
        &self,
        offset: u64,
        dest: &mut [u8],
    ) -> Result<(), TlvcReadError> {
        let addr: u32 = self.base + u32::try_from(offset).unwrap_lite();
        self.qspi.read_memory(addr, dest);
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    sys.enable_clock(sys_api::Peripheral::QuadSpi);
    sys.leave_reset(sys_api::Peripheral::QuadSpi);

    let reg = unsafe { &*device::QUADSPI::ptr() };
    let qspi = Qspi::new(reg, QSPI_IRQ);

    let clock = 5; // 200MHz kernel / 5 = 40MHz clock
    qspi.configure(clock, 24); // 2**24 = 16MiB = 128Mib

    // Sidecar-only for now!
    //
    // This is mostly copied from `gimlet-hf-server`, with a few pin adjustments
    //
    // SP_QSPI_RESET_L     PF5     GPIO
    // SP_QSPI_CLK         PF10    QUADSPI_CLK
    // SP_QSPI_IO0 (SI)    PF8     QUADSPI_BK1_IO0
    // SP_QSPI_IO1 (SO)    PF9     QUADSPI_BK1_IO1
    // SP_QSPI_CS_L        PG6     QUADSPI_BK1_NCS (or GPIO?)
    // SP_QSPI_IO2 (*WP)   PF7     QUADSPI_BK1_IO2
    // SP_QSPI_IO3 (*HOLD) PF6     QUADSPI_BK1_IO3
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(6).and_pin(7).and_pin(10),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF9,
    )
    .unwrap();
    sys.gpio_configure_alternate(
        sys_api::Port::F.pin(8).and_pin(9),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    )
    .unwrap();
    sys.gpio_configure_alternate(
        sys_api::Port::G.pin(6),
        sys_api::OutputType::PushPull,
        sys_api::Speed::Medium,
        sys_api::Pull::None,
        sys_api::Alternate::AF10,
    )
    .unwrap();

    let qspi_reset = sys_api::Port::F.pin(5);
    sys.gpio_reset(qspi_reset).unwrap();
    sys.gpio_configure_output(
        qspi_reset,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    )
    .unwrap();

    // TODO: The best clock frequency to use can vary based on the flash
    // part, the command used, and signal integrity limits of the board.

    // Ensure hold time for reset in case we just restarted.
    // TODO look up actual hold time requirement
    hl::sleep_for(1);

    // Release reset and let it stabilize.
    sys.gpio_set(qspi_reset).unwrap();
    hl::sleep_for(10);

    // TODO: check the ID and make sure it's what we expect
    //
    // Gimlet is  MT25QU256ABA8E12
    // Sidecar is S25FL128SAGMFIR01
    let mut buffer = [0; idl::INCOMING_SIZE];
    let mut server = ServerImpl { qspi };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

////////////////////////////////////////////////////////////////////////////////

struct ServerImpl {
    qspi: Qspi,
}

impl ServerImpl {
    /// Polls for the "Write Complete" flag.
    ///
    /// Sleep times are in ticks (typically milliseconds) and are somewhat
    /// experimentally determined, see hubris#753 for details.
    fn poll_for_write_complete(&self, sleep: Option<u64>) {
        loop {
            let status = self.qspi.read_status();
            if status & 1 == 0 {
                // ooh we're done
                break;
            }
            if let Some(sleep) = sleep {
                hl::sleep_for(sleep);
            }
        }
    }

    fn set_and_check_write_enable(&self) -> Result<(), AuxFlashError> {
        self.qspi.write_enable();
        let status = self.qspi.read_status();

        if status & 0b10 == 0 {
            // oh oh
            return Err(AuxFlashError::WriteEnableFailed);
        }
        Ok(())
    }
}

impl idl::InOrderAuxFlashImpl for ServerImpl {
    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<AuxFlashId, RequestError<AuxFlashError>> {
        let mut idbuf = [0; 20];
        self.qspi.read_id(&mut idbuf);
        Ok(AuxFlashId(idbuf))
    }

    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<AuxFlashError>> {
        Ok(self.qspi.read_status())
    }

    fn slot_count(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u32, RequestError<AuxFlashError>> {
        Ok(SLOT_COUNT)
    }

    fn read_slot_chck(
        &mut self,
        _: &RecvMessage,
        slot: u32,
    ) -> Result<AuxFlashChecksum, RequestError<AuxFlashError>> {
        if slot >= SLOT_COUNT {
            return Err(AuxFlashError::InvalidSlot.into());
        }
        let handle = SlotReader {
            qspi: &self.qspi,
            base: slot * SLOT_SIZE,
        };
        let mut reader = TlvcReader::begin(handle)
            .map_err(|_| AuxFlashError::TlvcReaderBeginFailed)?;

        let mut chck_expected = None;
        let mut chck_actual = None;
        while let Ok(Some(chunk)) = reader.next() {
            if &chunk.header().tag == b"CHCK" {
                if chck_expected.is_some() {
                    return Err(AuxFlashError::MultipleChck.into());
                } else if chunk.len() != 32 {
                    return Err(AuxFlashError::BadChckSize.into());
                }
                let mut out = [0; 32];
                chunk
                    .read_exact(0, &mut out)
                    .map_err(|_| AuxFlashError::ChunkReadFail)?;
                chck_expected = Some(out);
            } else if &chunk.header().tag == b"AUXI" {
                if chck_actual.is_some() {
                    return Err(AuxFlashError::MultipleAuxi.into());
                }
                let mut sha = Sha3_256::new();
                let mut scratch = [0u8; 256];
                let mut i: u64 = 0;
                while i < chunk.len() {
                    let amount = (chunk.len() - i).min(scratch.len() as u64);
                    chunk
                        .read_exact(i, &mut scratch[0..(amount as usize)])
                        .map_err(|_| AuxFlashError::ChunkReadFail)?;
                    i += amount as u64;
                    sha.update(&scratch[0..(amount as usize)]);
                }
                let mut out = [0; 32];
                let sha_out = sha.finalize();
                out.copy_from_slice(sha_out.as_slice());
                chck_actual = Some(out);
            }
        }
        match (chck_expected, chck_actual) {
            (None, _) => Err(AuxFlashError::MissingChck.into()),
            (_, None) => Err(AuxFlashError::MissingChck.into()),
            (Some(a), Some(b)) => {
                if a == b {
                    Err(AuxFlashError::ChckMismatch.into())
                } else {
                    Ok(AuxFlashChecksum(chck_expected.unwrap()))
                }
            }
        }
    }

    fn erase_slot(
        &mut self,
        _: &RecvMessage,
        slot: u32,
    ) -> Result<(), RequestError<AuxFlashError>> {
        if slot >= SLOT_COUNT {
            return Err(AuxFlashError::InvalidSlot.into());
        }
        let mem_start = slot * SLOT_SIZE;
        let mem_end = mem_start + SLOT_SIZE;

        let mut addr = mem_start;
        while addr < mem_end {
            self.set_and_check_write_enable()?;
            self.qspi.sector_erase(addr);
            addr += 64 * 1024;
            self.poll_for_write_complete(Some(1));
        }
        Ok(())
    }

    fn write_slot_with_offset(
        &mut self,
        _: &RecvMessage,
        slot: u32,
        offset: u32,
        data: LenLimit<Leased<R, [u8]>, 2048>,
    ) -> Result<(), RequestError<AuxFlashError>> {
        if offset + data.len() as u32 > SLOT_SIZE {
            return Err(AuxFlashError::AddressOverflow.into());
        }
        let mut addr = (slot * SLOT_SIZE + offset) as usize;
        let end = addr as usize + data.len();

        // Write in 256-byte chunks to minimize stack size
        let mut buf = [0u8; 256];
        let mut read = 0;
        while addr < end {
            let amount = (end - addr).min(buf.len());
            data.read_range(read..(read + amount), &mut buf[..amount])
                .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

            self.set_and_check_write_enable()?;
            self.qspi.page_program(addr as u32, &buf[..amount]);
            self.poll_for_write_complete(None);
            addr += amount;
            read += amount;
        }
        Ok(())
    }

    fn read_slot_with_offset(
        &mut self,
        _: &RecvMessage,
        slot: u32,
        offset: u32,
        dest: LenLimit<Leased<W, [u8]>, 256>,
    ) -> Result<(), RequestError<AuxFlashError>> {
        if offset + dest.len() as u32 > SLOT_SIZE {
            return Err(AuxFlashError::AddressOverflow.into());
        }

        let mut buf = [0u8; 256];
        let addr = slot * SLOT_SIZE + offset;
        self.qspi.read_memory(addr, &mut buf[..dest.len()]);
        dest.write_range(0..dest.len(), &buf[..dest.len()])
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;
        Ok(())
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::AuxFlashError;
    use drv_auxflash_api::{AuxFlashChecksum, AuxFlashId};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
