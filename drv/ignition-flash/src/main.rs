// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Grapefruit FPGA process.

#![no_std]
#![no_main]

use derive_idol_err::IdolError;
use drv_spi_api::{SpiDevice, SpiServer};
use idol_runtime::{
    ClientError, Leased, LenLimit, NotificationHandler, RequestError, R, W,
};
use userlib::{hl, task_slot, FromPrimitive, RecvMessage};

const PAGE_SIZE_BYTES: usize = 256;

task_slot!(SPI_FRONT, spi_front);
task_slot!(LOADER, spartan7_loader);

#[allow(unused)]
struct ServerImpl<'a, S: SpiServer> {
    dev: &'a SpiDevice<S>,
    ping: &'static mut [u8; 512],
    pong: &'static mut [u8; 512],
    seq: fmc_periph::Sequencer,
    is_selected: bool,
}

#[derive(
    Copy, Clone, Debug, FromPrimitive, Eq, PartialEq, IdolError, counters::Count,
)]
pub enum IgnitionFlashError {
    SpiError = 1,
    NotSelected,

    /// Server restarted
    #[idol(server_death)]
    TaskRestarted,
}

impl From<drv_spi_api::SpiError> for IgnitionFlashError {
    fn from(_value: drv_spi_api::SpiError) -> Self {
        IgnitionFlashError::SpiError
    }
}

pub enum Command {
    ReadStatusReg = 0x05,
    WriteEnable = 0x06,

    // Single-IO variants, compared with `drv_qspi_api`
    PageProgram = 0x02,
    Read = 0x03,

    ReadId = 0x9F,

    BulkErase = 0xC7,
    SectorErase = 0xDC,
}

#[export_name = "main"]
fn main() -> ! {
    let spi_front = drv_spi_api::Spi::from(SPI_FRONT.get_task_id());
    let dev = spi_front.device(drv_spi_api::devices::MUX);
    let (ping, pong) = mutable_statics::mutable_statics! {
        static mut PING: [u8; 512] = [Default::default; _];
        static mut PONG: [u8; 512] = [Default::default; _];
    };

    // Wait for the Spartan-7 to be loaded, then update its checksum registers
    let loader =
        drv_spartan7_loader_api::Spartan7Loader::from(LOADER.get_task_id());
    let token = loader.get_token();
    let seq = fmc_periph::Sequencer::new(token);

    // Start with ignition flash deselected
    seq.ignition_control
        .modify(|r| r.set_mux_to_ignition(false));
    let mut server = ServerImpl {
        dev: &dev,
        ping,
        pong,
        seq,
        is_selected: false,
    };
    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

impl<S: SpiServer> idl::InOrderIgnitionFlashImpl for ServerImpl<'_, S> {
    fn select(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        self.set_selected(true);
        Ok(())
    }

    fn deselect(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        self.set_selected(false);
        Ok(())
    }

    fn read_id(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 20], RequestError<IgnitionFlashError>> {
        self.check_selected()?;
        let out = self.read_impl(Command::ReadId, None, 20)?;
        Ok(out.try_into().unwrap())
    }

    fn read_status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<IgnitionFlashError>> {
        self.check_selected()?;
        let out = self.read_status()?;
        Ok(out)
    }

    fn bulk_erase(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<IgnitionFlashError>> {
        self.check_selected()?;
        self.write_enable()?;
        self.dev
            .write(&[Command::BulkErase as u8])
            .map_err(IgnitionFlashError::from)
            .map_err(RequestError::from)?;
        self.poll_for_write_complete(0, 100)?;
        Ok(())
    }

    fn page_program(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        data: LenLimit<Leased<R, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<IgnitionFlashError>> {
        self.check_selected()?;
        self.write_enable()?;
        self.ping[0] = Command::PageProgram as u8;
        self.ping[1..4].copy_from_slice(&addr.to_be_bytes()[1..]);
        let n = data.len();
        data.read_range(0..n, &mut self.ping[4..][..n])
            .map_err(|_| RequestError::Fail(ClientError::BadLease))?;
        self.dev
            .write(&self.ping[..4 + n])
            .map_err(IgnitionFlashError::from)?;
        self.poll_for_write_complete(32, 1)?;
        Ok(())
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        addr: u32,
        dest: LenLimit<Leased<W, [u8]>, PAGE_SIZE_BYTES>,
    ) -> Result<(), RequestError<IgnitionFlashError>> {
        self.check_selected()?;
        let buf = self.read_impl(Command::Read, Some(addr), dest.len())?;

        // Copy from our internal buffer back to the lease
        dest.write_range(0..dest.len(), buf)
            .map_err(|_| RequestError::Fail(ClientError::WentAway))?;

        Ok(())
    }

    fn sector_erase(
        &mut self,
        _: &RecvMessage,
        addr: u32,
    ) -> Result<(), RequestError<IgnitionFlashError>> {
        self.check_selected()?;
        self.write_enable()?;
        self.ping[0] = Command::SectorErase as u8;
        self.ping[1..4].copy_from_slice(&addr.to_be_bytes()[1..]);
        self.dev
            .write(&self.ping[..4])
            .map_err(IgnitionFlashError::from)?;
        self.poll_for_write_complete(0, 1)?;
        Ok(())
    }
}

impl<S: SpiServer> ServerImpl<'_, S> {
    fn set_selected(&mut self, selected: bool) {
        if self.is_selected != selected {
            self.seq
                .ignition_control
                .modify(|r| r.set_mux_to_ignition(selected));
            self.is_selected = selected;
        }
    }

    fn check_selected(&self) -> Result<(), IgnitionFlashError> {
        if self.is_selected {
            Ok(())
        } else {
            Err(IgnitionFlashError::NotSelected)
        }
    }

    fn read_status(&mut self) -> Result<u8, IgnitionFlashError> {
        let out = self.read_impl(Command::ReadStatusReg, None, 1)?;
        Ok(out[0])
    }

    fn poll_for_write_complete(
        &mut self,
        busy_wait_count: usize,
        sleep_between_polls: u64,
    ) -> Result<(), IgnitionFlashError> {
        let mut i = 0;
        loop {
            let status = self.read_status()?;
            if status & 1 == 0 {
                // ooh we're done
                break;
            }
            if i < busy_wait_count {
                i += 1;
            } else {
                hl::sleep_for(sleep_between_polls);
            }
        }
        Ok(())
    }

    fn read_impl(
        &mut self,
        command: Command,
        addr: Option<u32>,
        count: usize,
    ) -> Result<&[u8], IgnitionFlashError> {
        self.ping[0] = command as u8;
        if let Some(addr) = addr {
            self.ping[1..4].copy_from_slice(&addr.to_be_bytes()[1..]);
        }
        self.dev
            .exchange(&self.ping[..4 + count], &mut self.pong[..4 + count])?;
        Ok(&self.pong[4..][..count])
    }

    fn write_enable(&mut self) -> Result<(), IgnitionFlashError> {
        self.dev.write(&[Command::WriteEnable as u8])?;
        Ok(())
    }
}

impl<S: SpiServer> NotificationHandler for ServerImpl<'_, S> {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        // Nothing to do here
    }
}

////////////////////////////////////////////////////////////////////////////////

mod idl {
    use super::IgnitionFlashError;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod fmc_periph {
    include!(concat!(env!("OUT_DIR"), "/fmc_sequencer.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
