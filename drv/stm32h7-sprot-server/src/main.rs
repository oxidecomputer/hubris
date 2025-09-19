// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]
#![deny(elided_lifetimes_in_paths)]

use attest_api::{
    AttestError, HashAlgorithm, NONCE_MAX_SIZE, NONCE_MIN_SIZE, TQ_HASH_SIZE,
};
use drv_lpc55_update_api::{
    RotBootInfo, RotComponent, RotPage, SlotId, SwitchDuration, UpdateTarget,
};
use drv_spi_api::{CsState, SpiDevice, SpiServer};
use drv_sprot_api::*;
use drv_stm32xx_sys_api as sys_api;
use hubpack::SerializedSize;
use idol_runtime::{NotificationHandler, RequestError};
use ringbuf::*;
use static_cell::ClaimOnceCell;
use sys_api::IrqControl;
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
    #[allow(unused)]
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
    RxBuf([u8; 16]),
    RotPage,
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
/// Slightly longer timeout
const TIMEOUT_MEDIUM: u32 = 50;
/// Long timeout
const TIMEOUT_LONG: u32 = 200;

// Delay between asserting CSn and sending the portion of a message
// that fits entirely in the RoT's FIFO.
const PART1_DELAY: u64 = 0;

// Delay between sending the portion of a message that fits entirely in the
// RoT's FIFO and the remainder of the message. This gives time for the RoT
// sprot task to respond to its interrupt.
const PART2_DELAY: u64 = 2; // Observed to be at least 2ms on gimletlet

const MAX_UPDATE_ATTEMPTS: u16 = 3;

// Time to wait for a dump
//
// This timeout is probably longer than it needs to be, but there is no real harm
// in this. The RoT stops the SP, takes the dump, and then replies. During halt
// the SP isn't ticking so there is no time advancement.
//
// On the flipside, we have learned via unintended experiment that 5ms is too short!
const DUMP_TIMEOUT: u32 = 1000;

// On Gemini, the STM32H753 is in a LQFP176 package with ROT_IRQ
// on pin2/PE3
use gpio_irq_pins::ROT_IRQ;

// We use spi3 on gimletlet and spi4 on gemini and gimlet.
// You should be able to move the RoT board between SPI3, SPI4, and SPI6
// without much trouble even though SPI3 is the preferred connector and
// SPI4 is connected to the NET board.
cfg_if::cfg_if! {
    if #[cfg(any(
            target_board = "gimlet-b",
            target_board = "gimlet-c",
            target_board = "gimlet-d",
            target_board = "gimlet-e",
            target_board = "gimlet-f",
            target_board = "sidecar-b",
            target_board = "sidecar-c",
            target_board = "sidecar-d",
            target_board = "psc-b",
            target_board = "psc-c",
            target_board = "gemini-bu-1",
            target_board = "grapefruit",
            target_board = "minibar",
            target_board = "cosmo-a",
            ))] {
        const ROT_SPI_DEVICE: u8 = drv_spi_api::devices::ROT;
        fn debug_config(_sys: &sys_api::Sys) { }
        fn debug_set(_sys: &sys_api::Sys, _asserted: bool) { }
    } else if #[cfg(target_board = "gimletlet-2")] {
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
        compile_error!("No configuration for ROT_SPI_DEVICE");
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
    tx_buf: &'static mut [u8; REQUEST_BUF_SIZE],
    rx_buf: &'static mut [u8; RESPONSE_BUF_SIZE],
}

#[export_name = "main"]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());
    let spi = claim_spi(&sys).device(ROT_SPI_DEVICE);

    sys.gpio_configure_input(ROT_IRQ, sys_api::Pull::Up);

    debug_config(&sys);

    let mut buffer = [0; idl::INCOMING_SIZE];
    let io = Io {
        sys,
        spi,
        stats: SpIoStats::default(),
    };
    let mut server = {
        static BUFS: ClaimOnceCell<(
            [u8; REQUEST_BUF_SIZE],
            [u8; RESPONSE_BUF_SIZE],
        )> =
            ClaimOnceCell::new(([0; REQUEST_BUF_SIZE], [0; RESPONSE_BUF_SIZE]));
        let (tx_buf, rx_buf) = BUFS.claim();
        ServerImpl { io, tx_buf, rx_buf }
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

        let part1_size = ROT_FIFO_SIZE;

        // Read the `Header`
        self.spi.read(&mut rx_buf[..part1_size])?;

        let (header, _) = hubpack::deserialize::<Header>(rx_buf)?;
        let total_size =
            Header::MAX_SIZE + header.body_size as usize + CRC_SIZE;
        let part2_size = total_size.saturating_sub(part1_size);

        // Allow RoT time to rouse itself.
        if PART2_DELAY != 0 {
            hl::sleep_for(PART2_DELAY);
        }

        if total_size > RESPONSE_BUF_SIZE {
            return Err(SprotProtocolError::BadMessageLength.into());
        }

        if part2_size > 0 {
            // Read part 2
            self.spi.read(&mut rx_buf[part1_size..total_size])?;
        }

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
    // We do this by asking the `sys` task to notify us when the GPIO pin's
    // state changes (using EXTI), and waiting for either that or timeout
    // determined based on `max_sleep`.
    fn wait_rot_irq(&mut self, desired: bool, max_sleep: u32) -> bool {
        use notifications::{sprot::TIMER_MASK, ROT_IRQ_MASK};
        // Determine our edge sensitivity for the interrupt. The ROT_IRQ line is
        // active low, so if we want to wait for it to be asserted, wait for the
        // falling edge. If the line is currently asserted, and we're waiting
        // for it to be *deasserted*, we want to wait for a rising edge.
        let sensitivity = match desired {
            false => sys_api::Edge::Rising,
            true => sys_api::Edge::Falling,
        };
        self.sys.gpio_irq_configure(ROT_IRQ_MASK, sensitivity);

        // Enable the interrupt.
        self.sys
            .gpio_irq_control(ROT_IRQ_MASK, IrqControl::Enable)
            // Just unwrap this, because the `sys` task should never panic.
            .unwrap_lite();

        // Determine the deadline after which we'll give up, and start the clock.
        set_timer_relative(max_sleep, TIMER_MASK);

        let mut irq_fired = false;
        while self.is_rot_irq_asserted() != desired {
            // Wait to be notified either by the timeout or by the ROT_IRQ pin
            // changing state.
            const MASK: u32 = TIMER_MASK | ROT_IRQ_MASK;
            let notif = sys_recv_notification(MASK);

            // First, check if the IRQ has fired. We do this by checking if the
            // ROT_IRQ notification bit has been posted, and then asking the
            // `sys` task to confirm that we actually got the IRQ.
            //
            // N.B. that we check this *before* checking for the timer
            // notification bit, because it's possible *both* notifications were
            // posted before we were scheduled again, and if the IRQ did fire,
            // we'd prefer to honor that.
            irq_fired = notif.check_condition(ROT_IRQ_MASK, || {
                self.sys
                    // If the IRQ hasn't fired, leave it enabled, otherwise,
                    // if it has fired, don't re-enable the IRQ.
                    .gpio_irq_control(ROT_IRQ_MASK, IrqControl::Check)
                    // Sys task shouldn't panic.
                    .unwrap_lite()
            });
            if irq_fired {
                break;
            }

            // If the timer notification was posted, and the GPIO IRQ
            // notification wasn't, we've waited for the timeout. Too bad!
            if notif.has_timer_fired(TIMER_MASK) {
                // Disable the IRQ, so that we don't get the notification later
                // while in `recv`.
                self.sys
                    .gpio_irq_control(
                        notifications::ROT_IRQ_MASK,
                        IrqControl::Disable,
                    )
                    .unwrap_lite();

                // Record the timeout.
                self.stats.timeouts = self.stats.timeouts.wrapping_add(1);
                ringbuf_entry!(Trace::RotReadyTimeout);
                return false;
            }
        }

        // Ensure the timer gets unset before returning, to reduce the
        // likelihood that we get an immediate wake on the TIMER notification
        // next time into this routine. (We might still get one, but for it to
        // occur the timer needs to go off between the recv above, and this
        // line.)
        sys_set_timer(None, TIMER_MASK);
        // If the IRQ didn't fire, let's also disable it, so that it also
        // doesn't go off later.
        if !irq_fired {
            self.sys
                .gpio_irq_control(
                    notifications::ROT_IRQ_MASK,
                    IrqControl::Disable,
                )
                .unwrap_lite();
        }

        // We return `true` here regardless of `irq_fired`, because we may not
        // have looped at all, if the line was asserted before we started
        // waiting for the IRQ. The timeout case returns early, above.
        true
    }
}

impl<S: SpiServer> ServerImpl<S> {
    fn do_send_recv_retries(
        &mut self,
        mut tx_size: usize,
        timeout: u32,
        retries: u16,
    ) -> Result<Response<'_>, SprotError> {
        let mut attempts_left = retries;

        // We must always send an even number of bytes since
        // the RoT waits for 2 bytes in each fifo entry before making data
        // available. Extra data in the data frame will be ignored on
        // deserialization.
        //
        // Our buffers must always be large enough to contain our data plus an
        // extra byte. Otherwise, this is a programmer error.
        if !tx_size.is_multiple_of(2) {
            tx_size += 1;
        }

        loop {
            let err = match self.io.do_send_recv(
                &self.tx_buf[..tx_size],
                &mut self.rx_buf[..],
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
                    &*(&self.rx_buf[..] as *const [u8])
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
                        ringbuf_entry!(Trace::RxBuf(
                            self.rx_buf[0..16].try_into().unwrap()
                        ));
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
        let tx_size = Request::pack(&ReqBody::Status, self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Status(rot_status) = rsp.body? {
            let sp_status = SpStatus {
                version: CURRENT_VERSION,
                min_version: MIN_VERSION,
                request_buf_size: REQUEST_BUF_SIZE.try_into().unwrap_lite(),
                response_buf_size: RESPONSE_BUF_SIZE.try_into().unwrap_lite(),
            };
            Ok(SprotStatus {
                rot: rot_status,
                sp: sp_status,
            })
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Return IO stats for the SP and RoT
    fn io_stats(
        &mut self,
        _: &RecvMessage,
    ) -> Result<SprotIoStats, RequestError<SprotError>> {
        let tx_size = Request::pack(&ReqBody::IoStats, self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::IoStats(rot_stats) = rsp.body? {
            Ok(SprotIoStats {
                rot: rot_stats,
                sp: self.io.stats,
            })
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Return boot info about the RoT - deprecated
    fn rot_state(
        &mut self,
        _: &RecvMessage,
    ) -> Result<RotState, RequestError<SprotError>> {
        let tx_size = Request::pack(&ReqBody::RotState, self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::RotState(info) = rsp.body? {
            Ok(info)
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Return more useful boot info about the RoT
    fn rot_boot_info(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<RotBootInfo, RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::BootInfo);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Update(UpdateRsp::BootInfo(boot_info)) = rsp.body? {
            Ok(boot_info)
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Return more useful boot info about the RoT
    fn versioned_rot_boot_info(
        &mut self,
        _msg: &userlib::RecvMessage,
        version: u8,
    ) -> Result<VersionedRotBootInfo, RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::VersionedBootInfo { version });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_QUICK,
            DEFAULT_ATTEMPTS,
        )?;
        if let RspBody::Update(UpdateRsp::VersionedBootInfo(vboot_info)) =
            rsp.body?
        {
            Ok(vboot_info)
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
        let tx_size = Request::pack(&body, self.tx_buf);
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
        let tx_size = Request::pack(&body, self.tx_buf);
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
        let tx_size = Request::pack_with_blob(&body, self.tx_buf, block)?;

        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_MEDIUM,
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
        let tx_size = Request::pack(&body, self.tx_buf);
        // For stage0next updates, erase and flash doesn't happen
        // until the finish operations. Use a long timeout.
        let rsp = self.do_send_recv_retries(
            tx_size,
            // TODO: Tune TIMEOUT_LONG and deal with retried finish_image_update.
            TIMEOUT_LONG,
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
        let tx_size = Request::pack(&body, self.tx_buf);
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
        let tx_size = Request::pack(&body, self.tx_buf);
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
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        if let RspBody::Ok = rsp.body? {
            Ok(())
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    /// Trigger a dump of the SP by the RoT
    fn dump(
        &mut self,
        _: &userlib::RecvMessage,
        addr: u32,
    ) -> Result<(), idol_runtime::RequestError<DumpOrSprotError>> {
        let body = ReqBody::Dump(DumpReq::V1 { addr });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, DUMP_TIMEOUT, 1)?;
        if let RspBody::Dump(DumpRsp::V1 { err }) = rsp.body? {
            err.map_or(Ok(()), |e| DumpOrSprotError::Dump(e).into())
        } else {
            Err(SprotError::Protocol(SprotProtocolError::UnexpectedResponse))?
        }
    }

    fn caboose_size(
        &mut self,
        _: &userlib::RecvMessage,
        slot: SlotId,
    ) -> Result<u32, idol_runtime::RequestError<RawCabooseOrSprotError>> {
        let body = ReqBody::Caboose(CabooseReq::Size { slot });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self
            .do_send_recv_retries(tx_size, DUMP_TIMEOUT, 1)
            .map_err(RawCabooseOrSprotError::Sprot)?;
        match rsp.body {
            Ok(RspBody::Caboose(Ok(CabooseRsp::Size(size)))) => Ok(size),
            Ok(RspBody::Caboose(Err(e))) => {
                Err(RawCabooseOrSprotError::Caboose(e).into())
            }
            Ok(RspBody::Caboose(_)) | Ok(_) => {
                Err(RawCabooseOrSprotError::Sprot(SprotError::Protocol(
                    SprotProtocolError::UnexpectedResponse,
                ))
                .into())
            }
            Err(e) => Err(RawCabooseOrSprotError::Sprot(e).into()),
        }
    }

    fn read_caboose_region(
        &mut self,
        _: &userlib::RecvMessage,
        offset: u32,
        slot: SlotId,
        data: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<RawCabooseOrSprotError>> {
        let body = ReqBody::Caboose(CabooseReq::Read {
            slot,
            start: offset,
            size: data.len() as u32,
        });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self
            .do_send_recv_retries(tx_size, DUMP_TIMEOUT, 4)
            .map_err(RawCabooseOrSprotError::Sprot)?;

        match rsp.body {
            Ok(RspBody::Caboose(Ok(CabooseRsp::Read))) => {
                // Copy from the trailing data into the lease
                if rsp.blob.len() < data.len() {
                    return Err(idol_runtime::RequestError::Fail(
                        idol_runtime::ClientError::BadLease,
                    ));
                }
                data.write_range(0..data.len(), &rsp.blob[..data.len()])
                    .map_err(|()| {
                        idol_runtime::RequestError::Fail(
                            idol_runtime::ClientError::WentAway,
                        )
                    })?;
                Ok(())
            }
            Ok(RspBody::Caboose(Err(e))) => {
                Err(RawCabooseOrSprotError::Caboose(e).into())
            }
            Ok(RspBody::Caboose(_)) | Ok(_) => {
                Err(RawCabooseOrSprotError::Sprot(SprotError::Protocol(
                    SprotProtocolError::UnexpectedResponse,
                ))
                .into())
            }
            Err(e) => Err(RawCabooseOrSprotError::Sprot(e).into()),
        }
    }

    fn cert(
        &mut self,
        _: &userlib::RecvMessage,
        index: u32,
        offset: u32,
        data: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::Cert {
            index,
            offset,
            size: data.len() as u32,
        });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self
            .do_send_recv_retries(tx_size, DUMP_TIMEOUT, DEFAULT_ATTEMPTS)
            .map_err(AttestOrSprotError::Sprot)?;

        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::Cert))) => {
                // Copy from the trailing data into the lease
                if rsp.blob.len() < data.len() {
                    return Err(idol_runtime::RequestError::Fail(
                        idol_runtime::ClientError::BadLease,
                    ));
                }
                data.write_range(0..data.len(), &rsp.blob[..data.len()])
                    .map_err(|()| {
                        idol_runtime::RequestError::Fail(
                            idol_runtime::ClientError::WentAway,
                        )
                    })?;
                Ok(())
            }
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn cert_chain_len(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u32, idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::CertChainLen);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::CertChainLen(s)))) => Ok(s),
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn cert_len(
        &mut self,
        _: &userlib::RecvMessage,
        index: u32,
    ) -> Result<u32, idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::CertLen(index));
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::CertLen(s)))) => Ok(s),
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn record(
        &mut self,
        _: &userlib::RecvMessage,
        algorithm: HashAlgorithm,
        data: idol_runtime::LenLimit<
            idol_runtime::Leased<idol_runtime::R, [u8]>,
            MAX_BLOB_SIZE,
        >,
    ) -> Result<(), idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::Record { algorithm });
        let tx_size = Request::pack_with_blob(&body, self.tx_buf, data)?;
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;

        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::Record))) => Ok(()),
            Ok(_) => Err(AttestOrSprotError::Sprot(SprotError::Protocol(
                SprotProtocolError::UnexpectedResponse,
            ))
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn read_rot_page(
        &mut self,
        _: &userlib::RecvMessage,
        page: RotPage,
        data: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        ringbuf_entry!(Trace::RotPage);
        let body = ReqBody::RotPage { page };
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(
            tx_size,
            TIMEOUT_MEDIUM,
            DEFAULT_ATTEMPTS,
        )?;

        match rsp.body {
            Ok(RspBody::Page(Ok(RotPageRsp::RotPage))) => {
                // Copy from the trailing data into the lease
                if rsp.blob.len() < data.len() {
                    return Err(idol_runtime::RequestError::Fail(
                        idol_runtime::ClientError::BadLease,
                    ));
                }
                data.write_range(0..data.len(), &rsp.blob[..data.len()])
                    .map_err(|()| {
                        idol_runtime::RequestError::Fail(
                            idol_runtime::ClientError::WentAway,
                        )
                    })?;
                Ok(())
            }
            Ok(RspBody::Page(_)) | Ok(_) => {
                Err(SprotProtocolError::UnexpectedResponse)?
            }
            Err(e) => Err(e.into()),
        }
    }

    fn log(
        &mut self,
        _msg: &userlib::RecvMessage,
        offset: u32,
        data: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::Log {
            offset,
            size: data.len() as u32,
        });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp =
            self.do_send_recv_retries(tx_size, DUMP_TIMEOUT, DEFAULT_ATTEMPTS)?;

        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::Log))) => {
                // Copy from the trailing data into the lease
                if rsp.blob.len() < data.len() {
                    return Err(idol_runtime::RequestError::Fail(
                        idol_runtime::ClientError::BadLease,
                    ));
                }
                data.write_range(0..data.len(), &rsp.blob[..data.len()])
                    .map_err(|()| {
                        idol_runtime::RequestError::Fail(
                            idol_runtime::ClientError::WentAway,
                        )
                    })?;
                Ok(())
            }
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn log_len(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<u32, idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::LogLen);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::LogLen(s)))) => Ok(s),
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn attest(
        &mut self,
        _msg: &userlib::RecvMessage,
        nonce: idol_runtime::LenLimit<
            idol_runtime::Leased<idol_runtime::R, [u8]>,
            NONCE_MAX_SIZE,
        >,
        dest: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<AttestOrSprotError>>
    where
        AttestOrSprotError: From<idol_runtime::ServerDeath>,
    {
        if nonce.len() < NONCE_MIN_SIZE {
            return Err(
                AttestOrSprotError::Attest(AttestError::BadLease).into()
            );
        }

        let nonce_size = u32::try_from(nonce.len()).unwrap_lite();
        let write_size = u32::try_from(dest.len()).unwrap_lite();

        let body = ReqBody::Attest(AttestReq::Attest {
            nonce_size,
            write_size,
        });
        let tx_size = Request::pack_with_cb(&body, self.tx_buf, |buf| {
            nonce
                .read_range(0..nonce.len(), buf)
                .map_err(|_| SprotProtocolError::TaskRestarted)?;
            Ok::<usize, idol_runtime::RequestError<AttestOrSprotError>>(
                nonce.len(),
            )
        })?;

        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_LONG, 1)?;

        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::Attest))) => {
                // Copy response data into the lease
                if rsp.blob.len() < dest.len() {
                    return Err(idol_runtime::RequestError::Fail(
                        idol_runtime::ClientError::BadLease,
                    ));
                }
                dest.write_range(0..dest.len(), &rsp.blob[..dest.len()])
                    .map_err(|()| {
                        idol_runtime::RequestError::Fail(
                            idol_runtime::ClientError::WentAway,
                        )
                    })?;
                Ok(())
            }
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn attest_len(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<u32, idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::AttestLen);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::AttestLen(s)))) => Ok(s),
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn enable_sp_slot_watchdog(
        &mut self,
        _msg: &userlib::RecvMessage,
        time_ms: u32,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Swd(SwdReq::EnableSpSlotWatchdog { time_ms });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        rsp.body?;
        Ok(())
    }

    fn disable_sp_slot_watchdog(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Swd(SwdReq::DisableSpSlotWatchdog);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        rsp.body?;
        Ok(())
    }

    fn sp_slot_watchdog_supported(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Swd(SwdReq::SpSlotWatchdogSupported);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        rsp.body?;
        Ok(())
    }

    fn component_caboose_size(
        &mut self,
        _msg: &userlib::RecvMessage,
        component: RotComponent,
        slot: SlotId,
    ) -> Result<u32, idol_runtime::RequestError<RawCabooseOrSprotError>> {
        let body =
            ReqBody::Caboose(CabooseReq::ComponentSize { component, slot });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self
            .do_send_recv_retries(tx_size, DUMP_TIMEOUT, 1)
            .map_err(RawCabooseOrSprotError::Sprot)?;
        match rsp.body {
            Ok(RspBody::Caboose(Ok(CabooseRsp::ComponentSize(size)))) => {
                Ok(size)
            }
            Ok(RspBody::Caboose(Err(e))) => {
                Err(RawCabooseOrSprotError::Caboose(e).into())
            }
            Ok(RspBody::Caboose(_)) | Ok(_) => {
                Err(RawCabooseOrSprotError::Sprot(SprotError::Protocol(
                    SprotProtocolError::UnexpectedResponse,
                ))
                .into())
            }
            Err(e) => Err(RawCabooseOrSprotError::Sprot(e).into()),
        }
    }

    fn component_read_caboose_region(
        &mut self,
        _msg: &userlib::RecvMessage,
        offset: u32,
        component: RotComponent,
        slot: SlotId,
        data: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<RawCabooseOrSprotError>> {
        let body = ReqBody::Caboose(CabooseReq::ComponentRead {
            component,
            slot,
            start: offset,
            size: data.len() as u32,
        });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self
            .do_send_recv_retries(tx_size, DUMP_TIMEOUT, 4)
            .map_err(RawCabooseOrSprotError::Sprot)?;

        match rsp.body {
            Ok(RspBody::Caboose(Ok(CabooseRsp::ComponentRead))) => {
                // Copy from the trailing data into the lease
                if rsp.blob.len() < data.len() {
                    return Err(idol_runtime::RequestError::Fail(
                        idol_runtime::ClientError::BadLease,
                    ));
                }
                data.write_range(0..data.len(), &rsp.blob[..data.len()])
                    .map_err(|()| {
                        idol_runtime::RequestError::Fail(
                            idol_runtime::ClientError::WentAway,
                        )
                    })?;
                Ok(())
            }
            Ok(RspBody::Caboose(Err(e))) => {
                Err(RawCabooseOrSprotError::Caboose(e).into())
            }
            Ok(RspBody::Caboose(_)) | Ok(_) => {
                Err(RawCabooseOrSprotError::Sprot(SprotError::Protocol(
                    SprotProtocolError::UnexpectedResponse,
                ))
                .into())
            }
            Err(e) => Err(RawCabooseOrSprotError::Sprot(e).into()),
        }
    }

    fn component_prep_image_update(
        &mut self,
        _msg: &userlib::RecvMessage,
        component: RotComponent,
        slot: SlotId,
    ) -> Result<(), idol_runtime::RequestError<SprotError>>
    where
        SprotError: From<idol_runtime::ServerDeath>,
    {
        let body =
            ReqBody::Update(UpdateReq::ComponentPrep { component, slot });
        let tx_size = Request::pack(&body, self.tx_buf);
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

    fn component_switch_default_image(
        &mut self,
        _msg: &userlib::RecvMessage,
        component: RotComponent,
        slot: SlotId,
        duration: SwitchDuration,
    ) -> Result<(), idol_runtime::RequestError<SprotError>> {
        let body = ReqBody::Update(UpdateReq::ComponentSwitchDefaultImage {
            component,
            slot,
            duration,
        });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp =
            self.do_send_recv_retries(tx_size, TIMEOUT_LONG, DEFAULT_ATTEMPTS)?;
        if let RspBody::Ok = rsp.body? {
            Ok(())
        } else {
            Err(SprotProtocolError::UnexpectedResponse)?
        }
    }

    fn lifecycle_state(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<
        drv_sprot_api::LifecycleState,
        idol_runtime::RequestError<StateOrSprotError>,
    > {
        let body = ReqBody::State(StateReq::LifecycleState);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self
            .do_send_recv_retries(tx_size, TIMEOUT_QUICK, DEFAULT_ATTEMPTS)
            .map_err(StateOrSprotError::Sprot)?;
        match rsp.body.map_err(StateOrSprotError::Sprot)? {
            RspBody::State(Ok(StateRsp::LifecycleState(d))) => Ok(d),
            RspBody::State(Err(e)) => Err(StateOrSprotError::State(e).into()),
            _ => Err(StateOrSprotError::Sprot(SprotError::Protocol(
                SprotProtocolError::UnexpectedResponse,
            ))
            .into()),
        }
    }

    fn tq_cert(
        &mut self,
        _: &userlib::RecvMessage,
        index: u32,
        offset: u32,
        data: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::TqCert {
            index,
            offset,
            size: data.len() as u32,
        });
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self
            .do_send_recv_retries(tx_size, DUMP_TIMEOUT, DEFAULT_ATTEMPTS)
            .map_err(AttestOrSprotError::Sprot)?;

        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::TqCert))) => {
                // Copy from the trailing data into the lease
                if rsp.blob.len() < data.len() {
                    return Err(idol_runtime::RequestError::Fail(
                        idol_runtime::ClientError::BadLease,
                    ));
                }
                data.write_range(0..data.len(), &rsp.blob[..data.len()])
                    .map_err(|()| {
                        idol_runtime::RequestError::Fail(
                            idol_runtime::ClientError::WentAway,
                        )
                    })?;
                Ok(())
            }
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn tq_cert_chain_len(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u32, idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::TqCertChainLen);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::TqCertChainLen(s)))) => Ok(s),
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn tq_cert_len(
        &mut self,
        _: &userlib::RecvMessage,
        index: u32,
    ) -> Result<u32, idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::TqCertLen(index));
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::TqCertLen(s)))) => Ok(s),
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn tq_sign(
        &mut self,
        _msg: &userlib::RecvMessage,
        hash: idol_runtime::LenLimit<
            idol_runtime::Leased<idol_runtime::R, [u8]>,
            TQ_HASH_SIZE,
        >,
        dest: idol_runtime::Leased<idol_runtime::W, [u8]>,
    ) -> Result<(), idol_runtime::RequestError<AttestOrSprotError>>
    where
        AttestOrSprotError: From<idol_runtime::ServerDeath>,
    {
        if hash.len() != TQ_HASH_SIZE {
            return Err(
                AttestOrSprotError::Attest(AttestError::BadLease).into()
            );
        }

        let write_size = u32::try_from(dest.len()).unwrap_lite();

        let body = ReqBody::Attest(AttestReq::TqSign { write_size });
        let tx_size = Request::pack_with_cb(&body, self.tx_buf, |buf| {
            hash.read_range(0..hash.len(), buf)
                .map_err(|_| SprotProtocolError::TaskRestarted)?;
            Ok::<usize, idol_runtime::RequestError<AttestOrSprotError>>(
                hash.len(),
            )
        })?;

        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_LONG, 1)?;

        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::TqSign))) => {
                // Copy response data into the lease
                if rsp.blob.len() < dest.len() {
                    return Err(idol_runtime::RequestError::Fail(
                        idol_runtime::ClientError::BadLease,
                    ));
                }
                dest.write_range(0..dest.len(), &rsp.blob[..dest.len()])
                    .map_err(|()| {
                        idol_runtime::RequestError::Fail(
                            idol_runtime::ClientError::WentAway,
                        )
                    })?;
                Ok(())
            }
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }

    fn tq_sign_len(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<u32, idol_runtime::RequestError<AttestOrSprotError>> {
        let body = ReqBody::Attest(AttestReq::TqSignLen);
        let tx_size = Request::pack(&body, self.tx_buf);
        let rsp = self.do_send_recv_retries(tx_size, TIMEOUT_QUICK, 1)?;
        match rsp.body {
            Ok(RspBody::Attest(Ok(AttestRsp::TqSignLen(s)))) => Ok(s),
            Ok(RspBody::Attest(Err(e))) => {
                Err(AttestOrSprotError::Attest(e).into())
            }
            Ok(RspBody::Attest(_)) | Ok(_) => Err(AttestOrSprotError::Sprot(
                SprotError::Protocol(SprotProtocolError::UnexpectedResponse),
            )
            .into()),
            Err(e) => Err(AttestOrSprotError::Sprot(e).into()),
        }
    }
}

impl<S: SpiServer> NotificationHandler for ServerImpl<S> {
    fn current_notification_mask(&self) -> u32 {
        // Neither our timer nor our GPIO IRQ notifications are needed while the
        // server is in recv.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

mod idl {
    use super::{
        AttestOrSprotError, DumpOrSprotError, HashAlgorithm, LifecycleState,
        PulseStatus, RawCabooseOrSprotError, RotBootInfo, RotComponent,
        RotPage, RotState, SlotId, SprotError, SprotIoStats, SprotStatus,
        StateOrSprotError, SwitchDuration, UpdateTarget, VersionedRotBootInfo,
    };

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
include!(concat!(env!("OUT_DIR"), "/gpio_irq_pins.rs"));
