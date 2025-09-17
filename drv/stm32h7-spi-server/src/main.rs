// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Idol server task for the STM32H7 SPI peripheral.
//!
//! This is a thin wrapper around `stm32h7-spi-server-core`, which can be used
//! both in this task and embedded into other tasks.

#![no_std]
#![no_main]

use core::convert::Infallible;

use drv_spi_api::*;
use idol_runtime::{
    LeaseBufReader, LeaseBufWriter, Leased, LenLimit, NotificationHandler,
    RequestError, R, W,
};
use userlib::*;

use drv_stm32h7_spi_server_core::SpiServerCore;
use drv_stm32xx_sys_api as sys_api;

task_slot!(SYS, sys);

// This lets us amortize the cost of the borrow syscalls for retrieving data
// from the caller. It doesn't appear to be useful to make this any larger than
// the FIFO depth; for simplicity we set:
const BUFSIZ: usize = 16;

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let core = drv_stm32h7_spi_server_core::declare_spi_core!(
        sys,
        notifications::SPI_IRQ_MASK
    );
    let mut server = ServerImpl { core };
    let mut incoming = [0u8; INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

struct ServerImpl {
    core: SpiServerCore,
}

impl InOrderSpiImpl for ServerImpl {
    fn recv_source(&self) -> Option<userlib::TaskId> {
        self.core.recv_source()
    }

    fn closed_recv_fail(&mut self) {
        self.core.closed_recv_fail()
    }

    fn read(
        &mut self,
        _: &RecvMessage,
        device_index: u8,
        dest: LenLimit<Leased<W, [u8]>, 65535>,
    ) -> Result<(), RequestError<SpiError>> {
        self.core
            .read::<LeaseBufWriter<_, BUFSIZ>>(
                device_index,
                dest.into_inner().into(),
            )
            .map_err(RequestError::from)
    }

    fn write(
        &mut self,
        _: &RecvMessage,
        device_index: u8,
        src: LenLimit<Leased<R, [u8]>, 65535>,
    ) -> Result<(), RequestError<SpiError>> {
        self.core
            .write::<LeaseBufReader<_, BUFSIZ>>(
                device_index,
                src.into_inner().into(),
            )
            .map_err(RequestError::from)
    }

    fn exchange(
        &mut self,
        _: &RecvMessage,
        device_index: u8,
        src: LenLimit<Leased<R, [u8]>, 65535>,
        dest: LenLimit<Leased<W, [u8]>, 65535>,
    ) -> Result<(), RequestError<SpiError>> {
        self.core
            .exchange::<LeaseBufReader<_, BUFSIZ>, LeaseBufWriter<_, BUFSIZ>>(
                device_index,
                src.into_inner().into(),
                dest.into_inner().into(),
            )
            .map_err(RequestError::from)
    }

    fn lock(
        &mut self,
        rm: &RecvMessage,
        devidx: u8,
        cs_state: CsState,
    ) -> Result<(), RequestError<Infallible>> {
        self.core
            .lock(rm.sender, devidx, cs_state)
            .map_err(|_| idol_runtime::ClientError::BadMessageContents.fail())
    }

    fn release(
        &mut self,
        rm: &RecvMessage,
    ) -> Result<(), RequestError<Infallible>> {
        self.core
            .release(rm.sender)
            .map_err(|_| idol_runtime::ClientError::BadMessageContents.fail())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
