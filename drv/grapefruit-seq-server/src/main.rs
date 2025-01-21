// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Grapefruit FPGA process.

#![no_std]
#![no_main]

use drv_cpu_seq_api::{PowerState, StateChangeReason};
use drv_spi_api::{SpiDevice, SpiServer};
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};
use sha3::{Digest, Sha3_256};
use task_jefe_api::Jefe;
use task_packrat_api::{CacheSetError, MacAddressBlock, Packrat, VpdIdentity};
use userlib::{
    hl, sys_recv_notification, task_slot, FromPrimitive, RecvMessage,
    UnwrapLite,
};

use ringbuf::{counted_ringbuf, ringbuf_entry, Count};

task_slot!(JEFE, jefe);

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    FpgaInit(#[count(children)] bool),
    StartFailed(#[count(children)] SeqError),
    ContinueBitstreamLoad(usize),
    WaitForDone,
    Programmed,
    MacsAlreadySet(MacAddressBlock),
    IdentityAlreadySet(VpdIdentity),

    #[count(skip)]
    None,
}

#[derive(Copy, Clone, PartialEq, Count)]
enum SeqError {
    AuxMissingBlob,
    AuxReadError(#[count(children)] drv_auxflash_api::AuxFlashError),
    AuxChecksumMismatch,
    SpiWrite(#[count(children)] drv_spi_api::SpiError),
    DoneTimeout,
}

counted_ringbuf!(Trace, 128, Trace::None);

task_slot!(SYS, sys);
task_slot!(SPI, spi);
task_slot!(AUXFLASH, auxflash);
task_slot!(PACKRAT, packrat);

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let spi = drv_spi_api::Spi::from(SPI.get_task_id());
    let aux = drv_auxflash_api::AuxFlash::from(AUXFLASH.get_task_id());

    // Populate packrat with dummy values, because talking to the EEPROM is hard
    let packrat = Packrat::from(PACKRAT.get_task_id());
    let macs = MacAddressBlock {
        base_mac: [0; 6],
        count: 0.into(),
        stride: 0,
    };
    match packrat.set_mac_address_block(macs) {
        Ok(()) => (),
        Err(CacheSetError::ValueAlreadySet) => {
            ringbuf_entry!(Trace::MacsAlreadySet(macs));
        }
    }
    let identity = VpdIdentity {
        serial: *b"GRAPEFRUIT ",
        part_number: *b"913-0000083",
        revision: 0,
    };
    match packrat.set_identity(identity) {
        Ok(()) => (),
        Err(CacheSetError::ValueAlreadySet) => {
            ringbuf_entry!(Trace::IdentityAlreadySet(identity));
        }
    }

    match ServerImpl::init(&sys, spi, aux) {
        // Set up everything nicely, time to start serving incoming messages.
        Ok(mut server) => {
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

#[allow(unused)]
struct ServerImpl<S: SpiServer> {
    jefe: Jefe,
    sys: sys_api::Sys,
    seq: SpiDevice<S>,
}

const FAULT_PIN_L: sys_api::PinSet = sys_api::Port::A.pin(15);

const FPGA_PROGRAM_L: sys_api::PinSet = sys_api::Port::B.pin(6);
const FPGA_INIT_L: sys_api::PinSet = sys_api::Port::B.pin(5);
const FPGA_CONFIG_DONE: sys_api::PinSet = sys_api::Port::B.pin(4);

const FPGA_LOGIC_RESET_L: sys_api::PinSet = sys_api::Port::I.pin(15);

impl<S: SpiServer + Clone> ServerImpl<S> {
    fn init(
        sys: &sys_api::Sys,
        spi: S,
        aux: drv_auxflash_api::AuxFlash,
    ) -> Result<Self, SeqError> {
        // Ensure the SP fault pin is configured as an open-drain output, and pull
        // it low to make the sequencer restart externally visible.
        sys.gpio_configure_output(
            FAULT_PIN_L,
            sys_api::OutputType::OpenDrain,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );
        sys.gpio_reset(FAULT_PIN_L);

        // Hold the user logic in reset until we've loaded the bitstream
        sys.gpio_reset(FPGA_LOGIC_RESET_L);
        sys.gpio_configure_output(
            FPGA_LOGIC_RESET_L,
            sys_api::OutputType::PushPull,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );

        // Configure the FPGA_INIT_L and FPGA_CONFIG_DONE lines as inputs
        sys.gpio_configure_input(FPGA_INIT_L, sys_api::Pull::None);
        sys.gpio_configure_input(FPGA_CONFIG_DONE, sys_api::Pull::None);

        // To allow for the possibility that we are restarting, rather than
        // starting, we take care during early sequencing to _not turn anything
        // off,_ only on. This means if it was _already_ on, the outputs should
        // not glitch.

        // To program the FPGA, we're using "slave serial" mode.
        //
        // See "7 Series FPGAs Configuration", UG470 (v1.17) for details,
        // as well as "Using a Microprocessor to Configure Xilinx 7 Series FPGAs
        // via Slave Serial or Slave SelectMAP Mode Application Note" (XAPP583)

        // Configure the PROGRAM_B line to the FPGA
        sys.gpio_set(FPGA_PROGRAM_L);
        sys.gpio_configure_output(
            FPGA_PROGRAM_L,
            sys_api::OutputType::OpenDrain,
            sys_api::Speed::Low,
            sys_api::Pull::None,
        );

        // Pulse PROGRAM_B low for 1 ms to reset the bitstream
        // (T_PROGRAM is 250 ns min, so this is fine)
        // https://docs.amd.com/r/en-US/ds189-spartan-7-data-sheet/XADC-Specifications
        sys.gpio_reset(FPGA_PROGRAM_L);
        hl::sleep_for(1);
        sys.gpio_set(FPGA_PROGRAM_L);

        // Wait for INIT_B to rise
        loop {
            let init = sys.gpio_read(FPGA_INIT_L) != 0;
            ringbuf_entry!(Trace::FpgaInit(init));
            if init {
                break;
            }

            // Do _not_ burn CPU constantly polling, it's rude. We could also
            // set up pin-change interrupts but we only do this once per power
            // on, so it seems like a lot of work.
            hl::sleep_for(2);
        }

        // Bind to the sequencer device on our SPI port
        let seq = spi.device(drv_spi_api::devices::FPGA);

        let blob = aux
            .get_blob_by_tag(*b"FPGA")
            .map_err(|_| SeqError::AuxMissingBlob)?;
        let mut scratch_buf = [0u8; 128];
        let mut pos = blob.start;
        let mut sha = Sha3_256::new();
        let mut decompressor = gnarle::Decompressor::default();
        while pos < blob.end {
            let amount = (blob.end - pos).min(scratch_buf.len() as u32);
            let chunk = &mut scratch_buf[0..(amount as usize)];
            aux.read_slot_with_offset(blob.slot, pos, chunk)
                .map_err(SeqError::AuxReadError)?;
            sha.update(&chunk);
            pos += amount;

            // Reborrow as an immutable chunk, then decompress
            let mut chunk = &scratch_buf[0..(amount as usize)];
            let mut decompress_buffer = [0; 512];

            while !chunk.is_empty() {
                let decompressed_chunk = gnarle::decompress(
                    &mut decompressor,
                    &mut chunk,
                    &mut decompress_buffer,
                );

                // The compressor may have encountered a partial run at the
                // end of the `chunk`, in which case `decompressed_chunk`
                // will be empty since more data is needed before output is
                // generated.
                if !decompressed_chunk.is_empty() {
                    // Write the decompressed bitstream to the FPGA over SPI
                    seq.write(decompressed_chunk)
                        .map_err(SeqError::SpiWrite)?;
                    ringbuf_entry!(Trace::ContinueBitstreamLoad(
                        decompressed_chunk.len()
                    ));
                }
            }
        }

        let sha_out: [u8; 32] = sha.finalize().into();
        if sha_out != gen::FPGA_BITSTREAM_CHECKSUM {
            // Reset the FPGA to clear the invalid bitstream
            sys.gpio_reset(FPGA_PROGRAM_L);
            hl::sleep_for(1);
            sys.gpio_set(FPGA_PROGRAM_L);

            return Err(SeqError::AuxChecksumMismatch);
        }

        // Wait for the FPGA to pull DONE high
        const DELAY_MS: u64 = 2;
        const TIMEOUT_MS: u64 = 250;
        let mut wait_time_ms = 0;
        while sys.gpio_read(FPGA_CONFIG_DONE) == 0 {
            ringbuf_entry!(Trace::WaitForDone);
            hl::sleep_for(DELAY_MS);
            wait_time_ms += DELAY_MS;
            if wait_time_ms > TIMEOUT_MS {
                return Err(SeqError::DoneTimeout);
            }
        }

        // Send 64 bonus clocks to complete the startup sequence (see "Clocking
        // to End of Startup" in UG470).
        seq.write(&[0u8; 8]).map_err(SeqError::SpiWrite)?;

        ringbuf_entry!(Trace::Programmed);

        let server = Self {
            sys: sys.clone(),
            jefe: Jefe::from(JEFE.get_task_id()),
            seq,
        };
        server.set_state_impl(PowerState::A2);

        // Clear the external fault now that we're about to start serving
        // messages and fewer things can go wrong.
        sys.gpio_set(FAULT_PIN_L);

        // Enable the user design
        sys.gpio_set(FPGA_LOGIC_RESET_L);

        Ok(server)
    }

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
impl<S: SpiServer + Clone> idl::InOrderSequencerImpl for ServerImpl<S> {
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
        Ok(())
    }

    fn read_fpga_regs(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u8; 64], RequestError<core::convert::Infallible>> {
        Ok([0; 64])
    }
}

impl<S: SpiServer> NotificationHandler for ServerImpl<S> {
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
    include!(concat!(env!("OUT_DIR"), "/grapefruit_fpga.rs"));
}
