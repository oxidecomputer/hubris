// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Dedicated task for loading the Spartan-7 FPGA bitstream
//!
//! This FPGA is used as a memory-mapped peripheral.  Once it's loaded, other
//! tasks interact with it by writing directly to memory addresses, so it's
//! important that it remains running without interruption.  As such, this task
//! is infallible once it enters the Idol runtime loop.

#![no_std]
#![no_main]

use core::num::NonZeroUsize;
use drv_spartan7_spi_program::{BitstreamLoader, Spartan7Error};
use drv_spi_api::{SpiDevice, SpiServer};
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};
use userlib::{
    hl, sys_get_timer, sys_recv_notification, task_slot, RecvMessage,
    UnwrapLite,
};

use ringbuf::{counted_ringbuf, ringbuf_entry, Count};

////////////////////////////////////////////////////////////////////////////////
// Select local vs server SPI communication

/// Claims the SPI core.
///
/// This function can only be called once, and will panic otherwise!
#[cfg(feature = "use-spi-core")]
pub fn claim_spi(
    sys: &sys_api::Sys,
) -> drv_stm32h7_spi_server_core::SpiServerCore {
    drv_stm32h7_spi_server_core::declare_spi_core!(
        sys.clone(),
        notifications::SPI_IRQ_MASK
    )
}

#[cfg(not(feature = "use-spi-core"))]
pub fn claim_spi(_sys: &sys_api::Sys) -> drv_spi_api::Spi {
    task_slot!(SPI, spi);
    drv_spi_api::Spi::from(SPI.get_task_id())
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    #[count(skip)]
    None,
    FpgaInit,
    FpgaInitFailed(#[count(children)] Spartan7Error),
    StartFailed(#[count(children)] LoaderError),
    ContinueBitstreamLoad(usize),
    WaitForDone,
    Programmed {
        load_time_ms: u64,
    },
}

#[derive(Copy, Clone, PartialEq, Count)]
enum LoaderError {
    AuxFlashError(#[count(children)] drv_auxflash_api::AuxFlashError),
    SpartanError(#[count(children)] Spartan7Error),
    AuxChecksumMismatch,
}

impl From<drv_auxflash_api::AuxFlashError> for LoaderError {
    fn from(v: drv_auxflash_api::AuxFlashError) -> Self {
        LoaderError::AuxFlashError(v)
    }
}

impl From<Spartan7Error> for LoaderError {
    fn from(v: Spartan7Error) -> Self {
        LoaderError::SpartanError(v)
    }
}

counted_ringbuf!(Trace, 128, Trace::None);

task_slot!(SYS, sys);
task_slot!(AUXFLASH, auxflash);

#[export_name = "main"]
fn main() -> ! {
    match init() {
        // Set up everything nicely, time to start serving incoming messages.
        Ok(()) => {
            let mut server = ServerImpl;
            let mut buffer = [0; idl::INCOMING_SIZE];
            loop {
                idol_runtime::dispatch(&mut buffer, &mut server);
            }
        }

        // Initializing the FPGA failed.
        Err(e) => {
            // Log that something's broken
            //
            // The main sequencer task has ownership over the fault pin and is
            // responsible for leaving it held low until the FPGA boots, so it
            // will be stuck low here.
            ringbuf_entry!(Trace::StartFailed(e));

            // All these moments will be lost in time, like tears in rain...
            // Time to die.
            loop {
                // Sleeping with all bits in the notification mask clear means
                // we should never be notified --- and if one never wakes up,
                // the difference between sleeping and dying seems kind of
                // irrelevant. But, `rustc` doesn't realize that this should
                // never return, we'll stick it in a `loop` anyway so the main
                // function can return `!`
                sys_recv_notification(0);
            }
        }
    }
}

struct ServerImpl;

task_config::task_config! {
    program_l: sys_api::PinSet,
    init_l: sys_api::PinSet,
    config_done: sys_api::PinSet,
    user_reset_l: sys_api::PinSet,
}

fn init() -> Result<(), LoaderError> {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let dev = claim_spi(&sys).device(drv_spi_api::devices::SPARTAN7_FPGA);
    let aux = drv_auxflash_api::AuxFlash::from(AUXFLASH.get_task_id());
    let start = sys_get_timer().now;

    // Translate from our magical task config to our desired type
    let pin_cfg = drv_spartan7_spi_program::Config {
        program_l: TASK_CONFIG.program_l,
        init_l: TASK_CONFIG.init_l,
        config_done: TASK_CONFIG.config_done,
        user_reset_l: TASK_CONFIG.user_reset_l,
    };

    ringbuf_entry!(Trace::FpgaInit);
    // On initial power up, the FPGA may not be listening right away, so
    // retry for 500 ms.
    let loader = retry_spartan7_init(
        &sys,
        &pin_cfg,
        &dev,
        NonZeroUsize::new(10).unwrap_lite(),
        50,
    )?;

    let sha_out = aux.get_compressed_blob_streaming(
        *b"SPA7",
        |chunk| -> Result<(), LoaderError> {
            loader.continue_bitstream_load(chunk)?;
            ringbuf_entry!(Trace::ContinueBitstreamLoad(chunk.len()));
            Ok(())
        },
    )?;

    if sha_out != gen::SPARTAN7_FPGA_BITSTREAM_CHECKSUM {
        // Reset the FPGA to clear the invalid bitstream
        sys.gpio_reset(pin_cfg.program_l);
        hl::sleep_for(1);
        sys.gpio_set(pin_cfg.program_l);

        return Err(LoaderError::AuxChecksumMismatch);
    }

    ringbuf_entry!(Trace::WaitForDone);
    loader.finish_bitstream_load()?;

    // We need to wait for a little while before other tasks can start talking
    // to FMC-based peripherals implemented in the FPGA.  This specific delay is
    // probably overkill, but it's known to work!
    hl::sleep_for(100);

    let now = sys_get_timer().now;
    ringbuf_entry!(Trace::Programmed {
        load_time_ms: now - start
    });

    Ok(())
}

fn retry_spartan7_init<'a, S: SpiServer>(
    sys: &'a sys_api::Sys,
    pin_cfg: &'a drv_spartan7_spi_program::Config,
    dev: &'a SpiDevice<S>,
    count: NonZeroUsize,
    delay_ms: u64,
) -> Result<BitstreamLoader<'a, S>, Spartan7Error> {
    let mut last_err = None;
    for _ in 0..count.get() {
        match BitstreamLoader::begin_bitstream_load(sys, pin_cfg, dev, true) {
            Ok(loader) => return Ok(loader),
            Err(e) => {
                ringbuf_entry!(Trace::FpgaInitFailed(e));
                last_err = Some(e);
                hl::sleep_for(delay_ms);
            }
        }
    }
    Err(last_err.unwrap_lite())
}

impl idl::InOrderSpartan7LoaderImpl for ServerImpl {
    fn ping(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        Ok(())
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod gen {
    include!(concat!(env!("OUT_DIR"), "/spartan7_fpga.rs"));
}
