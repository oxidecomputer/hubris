// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]
#![deny(elided_lifetimes_in_paths)]

use core::convert::Into;
use drv_spi_api::{CsState, SpiDevice, SpiServer};
use drv_sprot_api::*;
use drv_stm32xx_sys_api as sys_api;
use drv_update_api::{SlotId, SwitchDuration, UpdateTarget};
use hubpack::SerializedSize;
use idol_runtime::RequestError;
use ringbuf::*;
use userlib::*;

cfg_if::cfg_if! {
    // Select local vs server SPI communication
    if #[cfg(feature = "use-spi-core")] {
        /// Claims the SPI core.
        ///
        /// This function can only be called once, and will panic otherwise!
        pub fn claim_spi(sys: &sys_api::Sys)
            -> drv_stm32h7_spi_server_core::SpiServerCore
        {
            drv_stm32h7_spi_server_core::declare_spi_core!(
                sys.clone(), notifications::SPI_IRQ_MASK)
        }
    } else {
        pub fn claim_spi(_sys: &sys_api::Sys) -> drv_spi_api::Spi {
            task_slot!(SPI, spi_driver);
            drv_spi_api::Spi::from(SPI.get_task_id())
        }
    }
}

task_slot!(SYS, sys);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    StatusReq,
    Debug(bool),
    Error(SprotError),
    FailedRetries {
        retries: u16,
        last_errcode: SprotError,
    },
    PulseFailed,
    Sent(usize),
    Received(usize),
    UnexpectedRotIrq,
    RotReadyTimeout,
    RspTimeout,
}
ringbuf!(Trace, 64, Trace::None);

// TODO:These timeouts are somewhat arbitrary.
// TODO: Make timeouts configurable
// All timeouts are in 'ticks'

/// Retry timeout for send_recv_retries
const RETRY_TIMEOUT: u64 = 5;

/// Timeout for status message
const TIMEOUT_QUICK: u32 = 5;
/// Default covers fail, pulse, retry
const DEFAULT_ATTEMPTS: u16 = 3;
// Tune the RoT flash write timeout
const TIMEOUT_WRITE_ONE_BLOCK: u32 = 50;

// Delay between asserting CSn and sending the portion of a message
// that fits entierly in the RoT's FIFO.
const PART1_DELAY: u64 = 0;

// Delay between sending the portion of a message that fits entirely in the
// RoT's FIFO and the remainder of the message. This gives time for the RoT
// sprot task to respond to its interrupt.
const PART2_DELAY: u64 = 2; // Observed to be at least 2ms on gimletlet

const MAX_UPDATE_ATTEMPTS: u16 = 3;

// ROT_IRQ comes from app.toml
// We use spi3 on gimletlet and spi4 on gemini and gimlet.
// You should be able to move the RoT board between SPI3, SPI4, and SPI6
// without much trouble even though SPI3 is the preferred connector and
// SPI4 is connected to the NET board.
cfg_if::cfg_if! {
    if #[cfg(any(
            target_board = "gimlet-b",
            target_board = "gimlet-c",
            target_board = "gimlet-d",
            target_board = "sidecar-b",
            target_board = "sidecar-c",
            target_board = "psc-a",
            target_board = "psc-b",
            target_board = "psc-c",
            target_board = "gemini-bu-1"
            ))] {
        const ROT_IRQ: sys_api::PinSet = sys_api::PinSet {
            // On Gemini, the STM32H753 is in a LQFP176 package with ROT_IRQ
            // on pin2/PE3
            port: sys_api::Port::E,
            pin_mask: 1 << 3,
        };
        const ROT_SPI_DEVICE: u8 = drv_spi_api::devices::ROT;
        fn debug_config(_sys: &sys_api::Sys) { }
        fn debug_set(_sys: &sys_api::Sys, _asserted: bool) { }
    } else if #[cfg(target_board = "gimletlet-2")] {
        const ROT_IRQ: sys_api::PinSet = sys_api::PinSet {
            port: sys_api::Port::D,
            pin_mask: 1 << 0,
        };
        const DEBUG_PIN: sys_api::PinSet = sys_api::PinSet {
            port: sys_api::Port::E,
            pin_mask: 1 << 6,
        };
        fn debug_config(sys: &sys_api::Sys) {
            sys.gpio_configure_output(
                DEBUG_PIN,
                sys_api::OutputType::OpenDrain,
                sys_api::Speed::High,
                sys_api::Pull::Up
            );
            debug_set(sys, true);
        }

        fn debug_set(sys: &sys_api::Sys, asserted: bool) {
            ringbuf_entry!(Trace::Debug(asserted));
            sys.gpio_set_to(DEBUG_PIN, asserted);
        }
        const ROT_SPI_DEVICE: u8 = drv_spi_api::devices::SPI3_HEADER;
    } else {
        compile_error!("No configuration for ROT_IRQ");
    }
}

// This is a separate type for IO, to prevent borrowing `ServerImpl` mutably
// and trying to return immutable slices from buffers.
pub struct Io<S: SpiServer> {
    stats: SpIoStats,
    sys: sys_api::Sys,
    spi: SpiDevice<S>,
}

pub struct ServerImpl<S: SpiServer> {
    io: Io<S>,
    tx_buf: [u8; MAX_REQUEST_SIZE],
    rx_buf: [u8; MAX_RESPONSE_SIZE],
}

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let spi = claim_spi(&sys).device(ROT_SPI_DEVICE);

    sys.gpio_configure_input(ROT_IRQ, sys_api::Pull::None);
    debug_config(&sys);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let io = Io {
        sys,
        spi,
        stats: SpIoStats::default(),
    };
    let mut server = ServerImpl {
        io,
        tx_buf: [0u8; MAX_REQUEST_SIZE],
        rx_buf: [0u8; MAX_RESPONSE_SIZE],
    };

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

impl<S: SpiServer> Io<S> {
    /// Handle the mechanics of sending a message and waiting for a response.
    fn do_send_recv(
        &mut self,
        tx_buf: &[u8],
        rx_buf: &mut [u8],
        timeout: u32,
    ) -> Result<usize, SprotError> {
        self.handle_rot_irq()?;
        self.do_send_request(tx_buf)?;

        if !self.wait_rot_irq(true, timeout) {
            ringbuf_entry!(Trace::RspTimeout);
            return Err(SprotProtocolError::Timeout.into());
        }

        // Fill in rx_buf with a complete message and validate its crc
        self.do_read_response(rx_buf)
    }

    // Send a request in 2 parts, with optional delays before each part.
    //
    // In order to improve reliability, start by sending only up to the first
    // ROT_FIFO_SIZE bytes and then delaying a short time. If the RoT is ready,
    // those first bytes will always fit in the RoT receive FIFO. Eventually,
    // the RoT FW will respond to the interrupt and enter a tight loop to
    // receive. The short delay should cover most of the lag in RoT interrupt
    // handling.
    fn do_send_request(&mut self, tx_buf: &[u8]) -> Result<(), SprotError> {
        // Increase the error count here. We'll decrease if we return successfully.
        self.stats.tx_errors = self.stats.tx_errors.wrapping_add(1);

        let part1_len = ROT_FIFO_SIZE.min(tx_buf.len());
        let (part1, part2) = tx_buf.split_at(part1_len);

        let _lock = self.spi.lock_auto(CsState::Asserted)?;
        if PART1_DELAY != 0 {
            hl::sleep_for(PART1_DELAY);
        }
        self.spi.write(part1)?;
        if !part2.is_empty() {
            if PART2_DELAY != 0 {
                hl::sleep_for(PART2_DELAY);
            }
            self.spi.write(part2)?;
        }
        // Remove the error that we added at the beginning of this function
        self.stats.tx_errors = self.stats.tx_errors.wrapping_sub(1);
        self.stats.tx_sent = self.stats.tx_sent.wrapping_add(1);

        ringbuf_entry!(Trace::Sent(tx_buf.len()));

        Ok(())
    }

    // Fetch as many bytes as we can and parse the header.
    // Return the size of the response read into rx_buf
    //
    // We can fetch FIFO size number of bytes reliably. After that, a short
    // delay and fetch the rest if there is a payload. Small messages will fit
    // entirely in the RoT FIFO.
    fn do_read_response(&self, rx_buf: &mut [u8]) -> Result<usize, SprotError> {
        let _lock = self.spi.lock_auto(CsState::Asserted)?;

        if PART1_DELAY != 0 {
            hl::sleep_for(PART1_DELAY);
        }

        let part1_size = Header::MAX_SIZE;

        // Read the `Header`
        self.spi.read(&mut rx_buf[..part1_size])?;

        let (header, _) = hubpack::deserialize::<Header>(&rx_buf)?;
        let part2_size = header.body_size as usize + CRC_SIZE;

        // Allow RoT time to rouse itself.
        hl::sleep_for(PART2_DELAY);
        let total_size = part1_size + part2_size;

        if total_size > MAX_RESPONSE_SIZE {
            return Err(SprotProtocolError::BadMessageLength.into());
        }

        // Read part 2
        self.spi.read(&mut rx_buf[part1_size..total_size])?;

        ringbuf_entry!(Trace::Received(total_size));

        Ok(total_size)
    }

    // TODO: Move README.md to RFD 317 and discuss:
    //   - Unsolicited messages from RoT to SP.
    //   - Ignoring message from RoT to SP.
    //   - Should we send a message telling RoT that SP has booted?
    //
    // TODO: The RoT must be able to observe SP resets. During the
    // normal start-up seqeunce, the RoT is controlling the SP's boot
    // up sequence. However, the SP can reset itself and individual
    // Hubris tasks may fail and be restarted.
    //
    // If SP and RoT are out of sync, e.g. this task restarts and an old
    // response is still in the RoT's transmit FIFO, then we can also see
    // ROT_IRQ asserted when not expected.
    //
    // Consider making configuration parameters for delays below
    fn handle_rot_irq(&mut self) -> Result<(), SprotError> {
        if self.is_rot_irq_asserted() {
            // See if the ROT_IRQ completes quickly.
            // This is the ROT_IRQ from the last request.
            if !self.wait_rot_irq(false, TIMEOUT_QUICK) {
                // Nope, it didn't complete. Pulse CSn.
                ringbuf_entry!(Trace::UnexpectedRotIrq);
                self.stats.csn_pulses += self.stats.csn_pulses.wrapping_add(1);
                // One sample of an LPC55S28 reacting to CSn deasserted
                // in about 54us. So, 10ms is plenty.
                if self.do_pulse_cs(10_u64, 10_u64)?.rot_irq_end == 1 {
                    // Did not clear ROT_IRQ
                    ringbuf_entry!(Trace::PulseFailed);
                    self.stats.csn_pulse_failures +=
                        self.stats.csn_pulse_failures.wrapping_add(1);
                    debug_set(&self.sys, false); // XXX
                    return Err(SprotProtocolError::RotIrqRemainsAsserted)?;
                }
            }
        }
        Ok(())
    }

    /// Clear the ROT_IRQ and the RoT's Tx buffer by toggling the CSn signal.
    /// ROT_IRQ before and after state is returned for testing.
    fn do_pulse_cs(
        &self,
        assert_ms: u64,
        delay_ms_after: u64,
    ) -> Result<PulseStatus, SprotError> {
        let rot_irq_begin = self.is_rot_irq_asserted();
        let lock = self
            .spi
            .lock_auto(CsState::Asserted)
            .map_err(|_| SprotProtocolError::CannotAssertCSn)?;
        if assert_ms != 0 {
            hl::sleep_for(assert_ms);
        }
        drop(lock);
        if delay_ms_after != 0 {
            hl::sleep_for(delay_ms_after);
        }
        let rot_irq_end = self.is_rot_irq_asserted();
        let status = PulseStatus {
            rot_irq_begin: u8::from(rot_irq_begin),
            rot_irq_end: u8::from(rot_irq_end),
        };
        Ok(status)
    }

    fn is_rot_irq_asserted(&self) -> bool {
        self.sys.gpio_read(ROT_IRQ) == 0
    }

    // Poll ROT_IRQ until asserted (true) or deasserted (false).
    //
    // We sleep and poll for what should be long enough for the RoT to queue
    // a response.
    //
    // TODO: Use STM32 EXTI as  an interrupt allows for better performance and
    // power efficiency.
    //
    // STM32 EXTI allows for 16 interrupts for GPIOs.
    // Each of those can represent Pin X from a GPIO bank (A through K)
    // So, only one bank's Pin 3, for example, can have the #3 interrupt.
    // For ROT_IRQ, we would configure for the falling edge to trigger
    // the interrupt. That configuration should be specified in the app.toml
    // for the board. Work needs to be done to generalize the EXTI facility.
    // But, hacking in one interrupt as an example should be ok to start things
    // off.
    fn wait_rot_irq(&mut self, desired: bool, max_sleep: u32) -> bool {
        let mut slept = 0;
        while self.is_rot_irq_asserted() != desired {
            if slept == max_sleep {
                self.stats.timeouts = self.stats.timeouts.wrapping_add(1);
                ringbuf_entry!(Trace::RotReadyTimeout);
                return false;
            }
            hl::sleep_for(1);
            slept += 1;
        }
        true
    }
}

impl<S: SpiServer> ServerImpl<S> {
    fn do_send_recv_retries(
        &mut self,
        tx_size: usize,
        timeout: u32,
        retries: u16,
    ) -> Result<Response<'_>, SprotError> {
        let mut attempts_left = retries;

        loop {
            let err = match self.io.do_send_recv(
                &self.tx_buf[..tx_size],
                &mut self.rx_buf,
                timeout,
            ) {
                // Recoverable errors dealing with our ability to receive
                // the message from the RoT.
                Err(err) => err,

                // The response itself may contain an error detected on the RoT
                // We use unsafe here to work around a bug in the NLL borrow
                // checker. See https://github.com/rust-lang/rust/issues/70255
                //
                // This is safe because we take an immutable reference to self.rx_buf
                // and we either return this reference, or it goes out of scope before
                // we take a mutable reference again at the top of the loop.
                Ok(_) => match Response::unpack(unsafe {
                    &*(&self.rx_buf as *const [u8])
                }) {
                    Ok(response) => {
                        self.io.stats.rx_received =
                            self.io.stats.rx_received.wrapping_add(1);
                        match response.body {
                            Ok(_) => return Ok(response),
                            Err(e) => e,
                        }
                    }
                    Err(err) => {
                        self.io.stats.rx_invalid =
                            self.io.stats.rx_invalid.wrapping_add(1);
                        err.into()
                    }
                },
            };

            ringbuf_entry!(Trace::Error(err));

            if !err.is_recoverable() {
                return Err(err);
            }

            self.io.stats.retries = self.io.stats.retries.wrapping_add(1);
            attempts_left -= 1;

            if attempts_left == 0 {
                ringbuf_entry!(Trace::FailedRetries {
                    retries,
                    last_errcode: err
                });
                return Err(err);
            }

            hl::sleep_for(RETRY_TIMEOUT);
        }
    }
}

impl<S: SpiServer> idl::InOrderSpRotImpl for ServerImpl<S> {
    /// Clear the RoT Tx buffer and have the RoT deassert ROT_IRQ.
    /// The status of ROT_IRQ before and after the assert is returned.
    ///
    /// If ROT_IRQ is asserted (a response is pending)
    /// ROT_IRQ should be deasserted in response to CSn pulse.
    fn pulse_cs(
        &mut self,
        _: &RecvMessage,
        delay: u16,
    ) -> Result<PulseStatus, RequestError<SprotError>> {
        self.io
            .do_pulse_cs(delay.into(), delay.into())
            .map_err(|e| e.into())
    }

    /// Retrieve status from the RoT.
    ///
    /// Use trusted interfaces when available. This is meant as
    /// an early or fallback source of information prior to stronger
    /// levels of trust being established.
    /// Having a signed StatusRsp is possible, but consider that carefully.
    fn status(
        &mut self,
        _: &RecvMessage,
    ) -> Result<SprotStatus, RequestError<SprotError>> {
        ringbuf_entry!(Trace::StatusReq);
        let tx_size = Request::pack(&ReqBody::Status, &mut self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Status(status) = rsp.body? {
            Ok(status)
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Return IO stats for the SP and RoT
    fn io_stats(
        &mut self,
        _: &RecvMessage,
    ) -> Result<IoStats, RequestError<SprotError>> {
        let tx_size = Request::pack(&ReqBody::IoStats, &mut self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::IoStats(rot_stats) = rsp.body? {
            Ok(IoStats {
                rot: rot_stats,
                sp: self.io.stats,
            })
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Return the block size of the update server
    fn block_size(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<u32, RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::GetBlockSize);
        let tx_size = Request::pack(&body, &mut self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Update(UpdateRsp::BlockSize(size)) = rsp.body? {
            Ok(size)
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Prepare an RoT update
    fn prep_image_update(
        &mut self,
        _msg: &userlib::RecvMessage,
        target: UpdateTarget,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::Prep(target));
        let tx_size = Request::pack(&body, &mut self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Ok = rsp.body? {
            Ok(())
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Write a block to the update server
    fn write_one_block(
        &mut self,
        _msg: &userlib::RecvMessage,
        block_num: u32,
        block: idol_runtime::LenLimit<
            idol_runtime::Leased<idol_runtime::R, [u8]>,
            MAX_BLOB_SIZE,
        >,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::WriteBlock { block_num });
        let tx_size = Request::pack_with_blob(&body, &mut self.tx_buf, block)?;

        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_WRITE_ONE_BLOCK,
            MAX_UPDATE_ATTEMPTS,
        )?;

        if let RspBody::Ok = rsp.body? {
            Ok(())
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    fn finish_image_update(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::Finish);
        let tx_size = Request::pack(&body, &mut self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Ok = rsp.body? {
            Ok(())
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    fn abort_update(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::Abort);
        let tx_size = Request::pack(&body, &mut self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Ok = rsp.body? {
            Ok(())
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    fn switch_default_image(
        &mut self,
        _msg: &userlib::RecvMessage,
        slot: SlotId,
        duration: SwitchDuration,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body =
            ReqBody::Update(UpdateReq::SwitchDefaultImage { slot, duration });
        let tx_size = Request::pack(&body, &mut self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Ok = rsp.body? {
            Ok(())
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Reset the RoT
    fn reset(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::Reset);
        let tx_size = Request::pack(&body, &mut self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        if let RspBody::Ok = rsp.body? {
            Ok(())
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }
}

mod idl {
    use super::{
        IoStats, PulseStatus, SlotId, SprotError, SprotStatus, SwitchDuration,
        UpdateTarget,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
