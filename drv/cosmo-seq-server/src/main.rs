// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Grapefruit FPGA process.

#![no_std]
#![no_main]

use drv_cpu_seq_api::{PowerState, StateChangeReason};
use drv_ice40_spi_program as ice40;
use drv_spartan7_loader_api::Spartan7Loader;
use drv_spi_api::{SpiDevice, SpiServer};
use drv_stm32xx_sys_api::{self as sys_api, Sys};
use idol_runtime::{NotificationHandler, RequestError};
use task_jefe_api::Jefe;
use userlib::{
    hl, sys_recv_notification, task_slot, FromPrimitive, RecvMessage,
    UnwrapLite,
};

use ringbuf::{counted_ringbuf, ringbuf_entry, Count};

task_slot!(JEFE, jefe);
task_slot!(LOADER, spartan7_loader);

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    FpgaInit,
    StartFailed(#[count(children)] SeqError),
    ContinueBitstreamLoad(usize),
    WaitForDone,
    Programmed,

    #[count(skip)]
    None,
}

#[derive(Copy, Clone, PartialEq, Count)]
enum SeqError {
    AuxFlashError(#[count(children)] drv_auxflash_api::AuxFlashError),
    AuxChecksumMismatch,
    Ice40(#[count(children)] ice40::Ice40Error),
}

impl From<drv_auxflash_api::AuxFlashError> for SeqError {
    fn from(v: drv_auxflash_api::AuxFlashError) -> Self {
        SeqError::AuxFlashError(v)
    }
}

counted_ringbuf!(Trace, 128, Trace::None);

task_slot!(SYS, sys);
task_slot!(SPI_FRONT, spi_front);
task_slot!(AUXFLASH, auxflash);

const SP_TO_SP5_NMI_SYNC_FLOOD_L: sys_api::PinSet = sys_api::Port::J.pin(2);
const SP_TO_IGN_TRGT_FPGA_FAULT_L: sys_api::PinSet = sys_api::Port::B.pin(7);
const SP_CHASSIS_STATUS_LED: sys_api::PinSet = sys_api::Port::C.pin(6);

#[export_name = "main"]
fn main() -> ! {
    // XXX set up fault pin
    match init() {
        // Set up everything nicely, time to start serving incoming messages.
        Ok(mut server) => {
            // Mark that we've reached A2, and turn on the chassis LED
            server.set_state_impl(PowerState::A2);
            server.sys.gpio_set(SP_CHASSIS_STATUS_LED);

            let mut buffer = [0; idl::INCOMING_SIZE];
            loop {
                idol_runtime::dispatch(&mut buffer, &mut server);
            }
        }

        // Initializing the sequencer failed.
        Err(e) => {
            // Tell everyone that something's broken, as loudly as possible.
            ringbuf_entry!(Trace::StartFailed(e));
            // Leave FAULT_PIN_L low (which is done at the start of init)

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

fn init() -> Result<ServerImpl, SeqError> {
    let sys = sys_api::Sys::from(SYS.get_task_id());

    // Pull the fault line low while we're loading
    sys.gpio_configure_output(
        SP_TO_IGN_TRGT_FPGA_FAULT_L,
        sys_api::OutputType::OpenDrain,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );
    sys.gpio_reset(SP_TO_IGN_TRGT_FPGA_FAULT_L);

    // Turn off the chassis LED, in case this is a task restart (and not a
    // full chip restart, which would leave the GPIO unconfigured).
    sys.gpio_configure_output(
        SP_CHASSIS_STATUS_LED,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );
    sys.gpio_reset(SP_CHASSIS_STATUS_LED);

    let spi_front = drv_spi_api::Spi::from(SPI_FRONT.get_task_id());
    let aux = drv_auxflash_api::AuxFlash::from(AUXFLASH.get_task_id());

    init_front_fpga(
        &sys,
        &spi_front.device(drv_spi_api::devices::MUX),
        &aux,
        &ice40::Config {
            creset: sys_api::Port::A.pin(4),
            cdone: sys_api::Port::A.pin(3),
        },
    )?;

    // Wait for the Spartan-7 to be loaded, which happens in parallel
    let loader = Spartan7Loader::from(LOADER.get_task_id());
    loader.ping();

    // Bring up the SP5 NMI pin
    sys.gpio_set(SP_TO_SP5_NMI_SYNC_FLOOD_L);
    sys.gpio_configure_output(
        SP_TO_SP5_NMI_SYNC_FLOOD_L,
        sys_api::OutputType::OpenDrain,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );

    // Clear the fault pin
    sys.gpio_set(SP_TO_IGN_TRGT_FPGA_FAULT_L);

    Ok(ServerImpl {
        jefe: Jefe::from(JEFE.get_task_id()),
        sys,
    })
}

#[allow(unused)]
struct ServerImpl {
    jefe: Jefe,
    sys: Sys,
}

/// Initialize the front FPGA, which is an ICE40
fn init_front_fpga<S: SpiServer>(
    sys: &sys_api::Sys,
    dev: &SpiDevice<S>,
    aux: &drv_auxflash_api::AuxFlash,
    config: &ice40::Config,
) -> Result<(), SeqError> {
    ringbuf_entry!(Trace::FpgaInit);
    ice40::begin_bitstream_load(dev, sys, config).map_err(SeqError::Ice40)?;

    let r = aux.get_compressed_blob_streaming(
        *b"ICE4",
        |chunk| -> Result<(), SeqError> {
            ice40::continue_bitstream_load(dev, chunk)
                .map_err(|e| SeqError::Ice40(ice40::Ice40Error::Spi(e)))?;
            ringbuf_entry!(Trace::ContinueBitstreamLoad(chunk.len()));
            Ok(())
        },
    );
    let _ = dev.release();
    let sha_out = r?;

    if sha_out != gen::FRONT_FPGA_BITSTREAM_CHECKSUM {
        // Drop the device into reset and hold it there
        sys.gpio_reset(config.creset);
        hl::sleep_for(1);
        let _ = dev.release();

        return Err(SeqError::AuxChecksumMismatch);
    }

    ringbuf_entry!(Trace::WaitForDone);
    ice40::finish_bitstream_load(dev, sys, config).map_err(SeqError::Ice40)?;
    ringbuf_entry!(Trace::Programmed);
    Ok(())
}

impl ServerImpl {
    fn get_state_impl(&self) -> PowerState {
        // Only we should be setting the state, and we set it to A2 on startup;
        // this conversion should never fail.
        PowerState::from_u32(self.jefe.get_state()).unwrap_lite()
    }

    fn set_state_impl(&self, state: PowerState) {
        self.jefe.set_state(state as u32);
    }

    fn validate_state_change(
        &self,
        state: PowerState,
    ) -> Result<(), drv_cpu_seq_api::SeqError> {
        match (self.get_state_impl(), state) {
            (PowerState::A2, PowerState::A0)
            | (PowerState::A0, PowerState::A2)
            | (PowerState::A0PlusHP, PowerState::A2)
            | (PowerState::A0Thermtrip, PowerState::A2) => Ok(()),

            _ => Err(drv_cpu_seq_api::SeqError::IllegalTransition),
        }
    }
}

// The `Sequencer` implementation for Grapefruit is copied from
// `mock-gimlet-seq-server`.  State is set to Jefe, but isn't actually
// controlled here.
impl idl::InOrderSequencerImpl for ServerImpl {
    fn get_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<PowerState, RequestError<core::convert::Infallible>> {
        Ok(self.get_state_impl())
    }

    fn set_state(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
    ) -> Result<(), RequestError<drv_cpu_seq_api::SeqError>> {
        self.validate_state_change(state)?;
        self.set_state_impl(state);
        Ok(())
    }

    fn set_state_with_reason(
        &mut self,
        _: &RecvMessage,
        state: PowerState,
        _: StateChangeReason,
    ) -> Result<(), RequestError<drv_cpu_seq_api::SeqError>> {
        self.validate_state_change(state)?;
        self.set_state_impl(state);
        Ok(())
    }

    fn send_hardware_nmi(
        &mut self,
        _: &RecvMessage,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        // The required length for an NMI pulse is apparently not documented.
        //
        // Let's try 25 ms!
        self.sys.gpio_reset(SP_TO_SP5_NMI_SYNC_FLOOD_L);
        hl::sleep_for(25);
        self.sys.gpio_set(SP_TO_SP5_NMI_SYNC_FLOOD_L);
        Ok(())
    }

    fn read_fpga_regs(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 64], RequestError<core::convert::Infallible>> {
        // XXX todo
        Ok([0; 64])
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
    use drv_cpu_seq_api::{SeqError, StateChangeReason};
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod gen {
    include!(concat!(env!("OUT_DIR"), "/cosmo_fpga.rs"));
}
