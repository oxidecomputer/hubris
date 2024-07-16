// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Grapefruit FPGA process.

#![no_std]
#![no_main]

use drv_spi_api::{SpiDevice, SpiServer};
use drv_stm32xx_sys_api as sys_api;
use idol_runtime::{NotificationHandler, RequestError};
use sha3::{Digest, Sha3_256};
use userlib::{hl, sys_recv_notification, task_slot, RecvMessage};

use ringbuf::{counted_ringbuf, ringbuf_entry, Count};

#[derive(Copy, Clone, PartialEq, Count)]
enum Trace {
    FpgaInit(#[count(children)] bool),
    StartFailed(#[count(children)] SeqError),
    ContinueBitstreamLoad(usize),
    WaitForDone,
    Programmed,
    #[count(skip)]
    None,
}

#[derive(Copy, Clone, PartialEq, Count)]
enum SeqError {
    AuxMissingBlob,
    AuxReadError(#[count(children)] drv_auxflash_api::AuxFlashError),
    AuxChecksumMismatch,
    SpiWrite(#[count(children)] drv_spi_api::SpiError),
}

counted_ringbuf!(Trace, 128, Trace::None);

task_slot!(SYS, sys);
task_slot!(SPI, spi);
task_slot!(AUXFLASH, auxflash);

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let spi = drv_spi_api::Spi::from(SPI.get_task_id());
    let aux = drv_auxflash_api::AuxFlash::from(AUXFLASH.get_task_id());

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

struct ServerImpl<S: SpiServer> {
    sys: sys_api::Sys,
    seq: SpiDevice<S>,
}

const FAULT_PIN_L: sys_api::PinSet = sys_api::Port::A.pin(15);

const FPGA_PROGRAM_L: sys_api::PinSet = sys_api::Port::B.pin(6);
const FPGA_INIT_L: sys_api::PinSet = sys_api::Port::B.pin(5);
const FPGA_CONFIG_DONE: sys_api::PinSet = sys_api::Port::B.pin(4);

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

        // Do we have to send the synchronization word ourself, or is it built
        // into the bitstream?
        // Same with device ID check
        // Load bitstream
        //
        // SP_TO_FPGA_CFG_CLK / SP_TO_FPGA_CFG_DAT
        // This is on SPI2, port B
        //
        // Wait for DONE (FPGA_TO_SP_CONFIG_DONE)

        // Bind to the sequencer device on our SPI port
        let seq = spi.device(drv_spi_api::devices::FPGA);

        // TODO do we need to send the bus width / synchronization word / device
        // ID ourselves, or are they built into the image?

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
                    seq.write(&decompressed_chunk)
                        .map_err(|e| SeqError::SpiWrite(e))?;
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
        while sys.gpio_read(FPGA_CONFIG_DONE) != 0 {
            ringbuf_entry!(Trace::WaitForDone);
            hl::sleep_for(2);
        }
        ringbuf_entry!(Trace::Programmed);

        let server = Self {
            sys: sys.clone(),
            seq,
        };

        // Clear the external fault now that we're about to start serving
        // messages and fewer things can go wrong.
        sys.gpio_set(FAULT_PIN_L);

        Ok(server)
    }
}

impl<S: SpiServer> idl::InOrderSequencerImpl for ServerImpl<S> {
    fn foo(
        &mut self,
        _: &RecvMessage,
    ) -> Result<u8, RequestError<core::convert::Infallible>> {
        Ok(1)
    }
}

impl<S: SpiServer> NotificationHandler for ServerImpl<S> {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        panic!()
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod gen {
    include!(concat!(env!("OUT_DIR"), "/grapefruit_fpga.rs"));
}
