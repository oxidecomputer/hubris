// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use core::ops::Range;

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use drv_stm32h7_usart as drv_usart;

use drv_gimlet_seq_api::{PowerState, SeqError, Sequencer};
use drv_usart::Usart;
use heapless::Vec;
use host_sp_messages::{
    Bsu, DecodeFailureReason, Header, HostToSp, HubpackError, SpToHost, Status,
    MAX_MESSAGE_SIZE,
};
use mutable_statics::mutable_statics;
use ringbuf::{ringbuf, ringbuf_entry};
use userlib::{
    hl, sys_get_timer, sys_irq_control, sys_recv_closed, sys_set_timer,
    task_slot, TaskId, UnwrapLite,
};

task_slot!(SYS, sys);
task_slot!(GIMLET_SEQ, gimlet_seq);

// TODO: When rebooting the host, we need to wait for the relevant power rails
// to decay. We ought to do this properly by monitoring the rails, but for now,
// we'll simply wait a fixed period of time. This time is a WAG - we should
// fix this!
const A2_REBOOT_DELAY: u64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Trace {
    None,
    UartTx(u8),
    UartTxFull,
    UartRx(u8),
    UartRxOverrun,
    ClearStatus { mask: u64 },
    SetState { now: u64, state: PowerState },
    JefeNotification { now: u64, state: PowerState },
}

ringbuf!(Trace, 64, Trace::None);

/// Notification bit for USART IRQ; must match configuration in app.toml.
const USART_IRQ: u32 = 1 << 0;

/// Notification bit for Jefe notifying us of state changes; must match Jefe's
/// `on-state-change` config for us in app.toml.
const JEFE_STATE_CHANGE_IRQ: u32 = 1 << 1;

/// Notification bit for the timer we set for ourselves.
const TIMER_IRQ: u32 = 1 << 2;

/// We set the high bit of the sequence number before replying to host requests.
const SEQ_REPLY: u64 = 0x8000_0000_0000_0000;

/// We wrap host/sp messages in corncobs; derive our max packet length from the
/// max unwrapped message length.
const MAX_PACKET_SIZE: usize = corncobs::max_encoded_len(MAX_MESSAGE_SIZE);

#[export_name = "main"]
fn main() -> ! {
    let mut server =
        ServerImpl::claim_static_resources(Status::SP_TASK_RESTARTED);

    sys_irq_control(USART_IRQ, true);

    loop {
        let mut mask = USART_IRQ | JEFE_STATE_CHANGE_IRQ;
        if let Some(RebootState::TransitionToA0At(deadline)) =
            server.reboot_state
        {
            sys_set_timer(Some(deadline), TIMER_IRQ);
            mask |= TIMER_IRQ;
        }

        // Wait for uart interrupt; if we haven't enabled tx interrupts, this
        // blocks until there's data to receive.
        let note = sys_recv_closed(&mut [], mask, TaskId::KERNEL)
            .unwrap_lite()
            .operation;

        server.handle_notification(note);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RebootState {
    // We've instructed the sequencer to transition to A2; we're waiting to see
    // the notification from jefe that that transition has occurred.
    WaitingForA2,
    // We're in our reboot delay (see `A2_REBOOT_DELAY`); once we're past this
    // deadline, we want to transition to A0.
    TransitionToA0At(u64),
}

struct ServerImpl {
    uart: Usart,
    tx_msg_buf: &'static mut [u8; MAX_MESSAGE_SIZE],
    tx_pkt_buf: &'static mut [u8; MAX_PACKET_SIZE],
    tx_pkt_to_write: Range<usize>,
    rx_buf: &'static mut Vec<u8, MAX_PACKET_SIZE>,
    status: Status,
    sequencer: Sequencer,
    reboot_state: Option<RebootState>,
}

impl ServerImpl {
    fn claim_static_resources(status: Status) -> Self {
        let (tx_msg_buf, tx_pkt_buf) = mutable_statics! {
                static mut UART_TX_MSG_BUF: [u8; MAX_MESSAGE_SIZE] = [0; _];
                static mut UART_TX_PKT_BUF: [u8; MAX_PACKET_SIZE] = [0; _];
        };
        Self {
            uart: configure_uart_device(),
            tx_msg_buf,
            tx_pkt_buf,
            tx_pkt_to_write: 0..0,
            rx_buf: claim_uart_rx_buf(),
            status,
            sequencer: Sequencer::from(GIMLET_SEQ.get_task_id()),
            reboot_state: None,
        }
    }

    /// Power off the host (i.e., transition to A2).
    ///
    /// If `reboot` is true and we successfully instruct the sequencer to
    /// transition to A2, we set `self.reboot_state` to
    /// `RebootState::WaitingForA2`. Once we receive the notification from Jefe
    /// that the transition is complete, we'll update that state to
    /// `RebootState::TransitionToA0At(_)`.
    ///
    /// If we're not able to instruct the sequencer to transition to A2, we ask
    /// the sequencer what the current state is and handle them
    /// hopefully-appropriately:
    ///
    /// 1. The sequencer reports the current state is A0 - our request to
    ///    transition to A2 should have succeeded, so presumably we hit a race
    ///    window. We retry.
    /// 2. We're already in A2 - the host is already powered off. If `reboot` is
    ///    true, we set `self.reboot_state` to
    ///    `RebootState::TransitionToA0At(_)` and will attempt to move back to
    ///    A0 once we pass that deadline.
    /// 3. We're in A1 - this state should be transitory, so we sleep and retry.
    // TODO is error handling in this method correct? I think we should
    // basically only ever succeed in our initial set_state() request, so I
    // don't know how we'd test it
    fn power_off_host(&mut self, reboot: bool) {
        loop {
            // Attempt to move to A2; given we only call this function in
            // response to a host request, we expect we're currently in A0 and
            // this should work.
            let err = match self.sequencer.set_state(PowerState::A2) {
                Ok(()) => {
                    ringbuf_entry!(Trace::SetState {
                        now: sys_get_timer().now,
                        state: PowerState::A2,
                    });
                    if reboot {
                        self.reboot_state = Some(RebootState::WaitingForA2);
                    }
                    return;
                }
                Err(err) => err,
            };

            // The only error we should see from `set_state()` is an illegal
            // transition, if we're not currently in A0.
            assert!(matches!(err, SeqError::IllegalTransition));

            // If we can't go to A2, what state are we in, keeping in mind that
            // we have a bit of TOCTOU here in that the state might've changed
            // since we tried to `set_state()` above?
            match self.sequencer.get_state().unwrap_lite() {
                // If we're in A0, we should've been able to transition to A2;
                // just repeat our loop and try again.
                PowerState::A0
                | PowerState::A0PlusHP
                | PowerState::A0Thermtrip => continue,

                // If we're already in A2 somehow, we're done.
                PowerState::A2
                | PowerState::A2PlusMono
                | PowerState::A2PlusFans => {
                    if reboot {
                        // Somehow we're already in A2 when the host wanted to
                        // reboot; set our reboot timer.
                        let reboot_at = sys_get_timer().now + A2_REBOOT_DELAY;
                        self.reboot_state =
                            Some(RebootState::TransitionToA0At(reboot_at));
                    }
                    return;
                }

                // A1 should be transitory; sleep then retry.
                PowerState::A1 => {
                    hl::sleep_for(1);
                    continue;
                }
            }
        }
    }

    fn handle_jefe_notification(&mut self, state: PowerState) {
        let now = sys_get_timer().now;
        ringbuf_entry!(Trace::JefeNotification { now, state });
        // If we're rebooting and jefe has notified us that we're now in A2,
        // move to A0. Otherwise, ignore this notification.
        match state {
            PowerState::A2
            | PowerState::A2PlusMono
            | PowerState::A2PlusFans => {
                // Were we waiting for a transition to A2? If so, start our
                // timer for going back to A0.
                if self.reboot_state == Some(RebootState::WaitingForA2) {
                    self.reboot_state = Some(RebootState::TransitionToA0At(
                        now + A2_REBOOT_DELAY,
                    ));
                }
            }
            PowerState::A1 => (), // do nothing
            PowerState::A0 | PowerState::A0PlusHP | PowerState::A0Thermtrip => {
                // TODO should we clear self.reboot_state here? What if we
                // transitioned from one A0 state to another? For now, leave it
                // set, and we'll move back to A0 whenever we transition to
                // A2...
            }
        }
    }

    fn handle_usart_notification(&mut self) {
        'tx: loop {
            // Do we have data to transmit? If so, write as much as we can until
            // either the fifo fills (in which case we return before trying to
            // receive more) or we finish flushing.
            while !self.tx_pkt_to_write.is_empty() {
                if try_tx_push(
                    &self.uart,
                    self.tx_pkt_buf[self.tx_pkt_to_write.start],
                ) {
                    self.tx_pkt_to_write.start += 1;
                } else {
                    return;
                }
            }

            // We're done flushing data; disable the tx fifo interrupt.
            self.uart.disable_tx_fifo_empty_interrupt();

            // Clear any RX overrun errors. If we hit this, we will likely fail
            // to decode the next message from the host, which will cause us to
            // send a `DecodeFailure` response.
            if self.uart.check_and_clear_rx_overrun() {
                ringbuf_entry!(Trace::UartRxOverrun);
            }

            // Receive until there's no more data or we get a 0x00, signifying
            // the end of a corncobs packet.
            while let Some(byte) = self.uart.try_rx_pop() {
                ringbuf_entry!(Trace::UartRx(byte));

                if byte == 0x00 {
                    // Process message and populate our response into
                    // `tx_msg_buf`.
                    let msg_len = self.process_message();
                    self.rx_buf.clear();

                    if let Some(msg_len) = msg_len {
                        // Encode our outgoing packet.
                        let pkt_len = corncobs::encode_buf(
                            &self.tx_msg_buf[..msg_len],
                            &mut self.tx_pkt_buf[..],
                        );
                        self.tx_pkt_to_write = 0..pkt_len;

                        // Enable tx fifo interrupts, and immediately start
                        // trying to send our response.
                        self.uart.enable_tx_fifo_empty_interrupt();
                        continue 'tx;
                    }
                } else if self.rx_buf.push(byte).is_err() {
                    // Message overflow - nothing we can do here except
                    // discard data. We'll drop this byte and wait til we
                    // see a 0 to respond, at which point our
                    // deserialization will presumably fail and we'll send
                    // back an error. Should we record that we overflowed
                    // here?
                }
            }

            // We received everything we could out of the rx fifo and we have
            // nothing to send; we're done.
            return;
        }
    }

    // Process the framed packet sitting in `self.rx_buf`. If it warrants a
    // response, we return `Some(n)` where n is the length of the outgoing,
    // unframed message we wrote to `self.tx_msg_buf`.
    fn process_message(&mut self) -> Option<usize> {
        let deframed = match corncobs::decode_in_place(self.rx_buf) {
            Ok(n) => &self.rx_buf[..n],
            Err(_) => {
                return Some(populate_with_decode_error(
                    self.tx_msg_buf,
                    DecodeFailureReason::Cobs,
                ));
            }
        };

        let (mut header, request, data) =
            match host_sp_messages::deserialize::<HostToSp>(deframed) {
                Ok((header, request, data)) => (header, request, data),
                Err(HubpackError::Custom) => {
                    return Some(populate_with_decode_error(
                        self.tx_msg_buf,
                        DecodeFailureReason::Crc,
                    ));
                }
                Err(_) => {
                    return Some(populate_with_decode_error(
                        self.tx_msg_buf,
                        DecodeFailureReason::Deserialize,
                    ));
                }
            };

        if header.magic != host_sp_messages::MAGIC {
            return Some(populate_with_decode_error(
                self.tx_msg_buf,
                DecodeFailureReason::MagicMismatch,
            ));
        }

        if header.version != host_sp_messages::version::V1 {
            return Some(populate_with_decode_error(
                self.tx_msg_buf,
                DecodeFailureReason::VersionMismatch,
            ));
        }

        if header.sequence & SEQ_REPLY != 0 {
            return Some(populate_with_decode_error(
                self.tx_msg_buf,
                DecodeFailureReason::SequenceInvalid,
            ));
        }

        // We defer any actions until after we've serialized our response to
        // avoid borrow checker issues with calling methods on `self`.
        let mut action = None;
        let mut response_data: &[u8] = &[];
        let response = match request {
            HostToSp::_Unused => {
                Some(SpToHost::DecodeFailure(DecodeFailureReason::Deserialize))
            }
            HostToSp::RequestReboot => {
                action = Some(Action::RebootHost);
                None
            }
            HostToSp::RequestPowerOff => {
                action = Some(Action::PowerOffHost);
                None
            }
            HostToSp::GetBootStorageUnit => {
                // TODO how do we know the real answer?
                Some(SpToHost::BootStorageUnit(Bsu::A))
            }
            HostToSp::GetIdentity => {
                // TODO how do we get our real identity?
                Some(SpToHost::Identity {
                    model: 1,
                    revision: 2,
                    serial: *b"fake-serial",
                })
            }
            HostToSp::GetMacAddresses => {
                // TODO where do we get host MAC addrs?
                Some(SpToHost::MacAddresses {
                    base: [0; 6],
                    count: 1,
                    stride: 1,
                })
            }
            HostToSp::HostBootFailure { .. } => {
                // TODO what do we do in reaction to this? reboot?
                Some(SpToHost::Ack)
            }
            HostToSp::HostPanic { .. } => {
                // TODO log event and/or forward to MGS
                Some(SpToHost::Ack)
            }
            HostToSp::GetStatus => Some(SpToHost::Status(self.status)),
            HostToSp::ClearStatus { mask } => {
                ringbuf_entry!(Trace::ClearStatus { mask });
                self.status &= Status::from_bits_truncate(!mask);
                Some(SpToHost::Status(self.status))
            }
            HostToSp::GetAlert { mask: _ } => {
                // TODO define alerts
                Some(SpToHost::Alert { action: 0 })
            }
            HostToSp::RotRequest => {
                // TODO forward request to RoT; for now just echo
                response_data = data;
                Some(SpToHost::RotResponse)
            }
            HostToSp::RotAddHostMeasurements => {
                // TODO forward request to RoT
                Some(SpToHost::Ack)
            }
            HostToSp::GetPhase2Data { start, count: _ } => {
                // TODO forward real data
                response_data = b"hello world";
                Some(SpToHost::Phase2Data { start })
            }
        };

        // We set the high bit of the sequence number before responding.
        header.sequence |= SEQ_REPLY;

        // TODO this can panic if `response_data` is too large; where do we
        // check it before sending a response?
        let tx_msg_buf = &mut self.tx_msg_buf; // borrow checker workaround
        let n = response.map(|response| {
            host_sp_messages::serialize(
                tx_msg_buf,
                &header,
                &response,
                response_data,
            )
            .unwrap_lite()
            .0
        });

        // Now that all buffer borrowing is done, we can borrow `self` mutably
        // again to perform any necessary action.
        if let Some(action) = action {
            match action {
                Action::RebootHost => self.power_off_host(true),
                Action::PowerOffHost => self.power_off_host(false),
            }
        }

        n
    }
}

// approximately idol_runtime::NotificationHandler, in anticipation of
// eventually having an idol interface (at least for mgmt-gateway to give us
// host phase 2 data)
impl ServerImpl {
    fn handle_notification(&mut self, bits: u32) {
        if bits & USART_IRQ != 0 {
            self.handle_usart_notification();
            sys_irq_control(USART_IRQ, true);
        }

        if bits & JEFE_STATE_CHANGE_IRQ != 0 {
            self.handle_jefe_notification(
                self.sequencer.get_state().unwrap_lite(),
            );
        }

        if bits & TIMER_IRQ != 0 {
            // If we're past the deadline for transitioning to A0, attempt to do
            // that.
            if let Some(RebootState::TransitionToA0At(deadline)) =
                self.reboot_state
            {
                if sys_get_timer().now >= deadline {
                    // The only way our reboot state gets set to
                    // `TransitionToA0At` is if we believe we were currently in
                    // A2. Attempt to transition to A0, which can only fail if
                    // we're no longer in A2. In either case (we successfully
                    // started the transition or we're no longer in A2 due to
                    // some external cause), we've done what we can to reboot,
                    // so clear out `reboot_state`.
                    ringbuf_entry!(Trace::SetState {
                        now: sys_get_timer().now,
                        state: PowerState::A0,
                    });
                    _ = self.sequencer.set_state(PowerState::A0);
                    self.reboot_state = None;
                }
            }
        }
    }
}

// Borrow checker workaround; list of actions we perform in response to a host
// request _after_ we're done borrowing any message buffers.
enum Action {
    RebootHost,
    PowerOffHost,
}

// wrapper around `usart.try_tx_push()` that registers the result in our
// ringbuf
fn try_tx_push(uart: &Usart, val: u8) -> bool {
    let ret = uart.try_tx_push(val);
    if ret {
        ringbuf_entry!(Trace::UartTx(val));
    } else {
        ringbuf_entry!(Trace::UartTxFull);
    }
    ret
}

fn populate_with_decode_error(
    out: &mut [u8; MAX_MESSAGE_SIZE],
    reason: DecodeFailureReason,
) -> usize {
    let header = Header {
        magic: host_sp_messages::MAGIC,
        version: host_sp_messages::version::V1,
        // We failed to decode, so don't know the sequence number.
        sequence: 0xffff_ffff_ffff_ffff,
    };
    let response = SpToHost::DecodeFailure(reason);

    // Serializing can only fail if we pass unexpected types as `response`, but
    // we're using `SpToHost`, so it cannot fail.
    host_sp_messages::serialize(out, &header, &response, &[])
        .unwrap_lite()
        .0
}

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
fn configure_uart_device() -> Usart {
    use drv_usart::device;
    use drv_usart::drv_stm32xx_sys_api::*;

    // TODO: this module should _not_ know our clock rate. That's a hack.
    const CLOCK_HZ: u32 = 100_000_000;

    // For gimlet, we only expect baud rate 3 Mbit, uart7, with hardware flow
    // control enabled. We could expand our cargo features to cover other cases
    // as needed. Currently, failing to enable any of those three features will
    // cause a compilation error.
    #[cfg(feature = "baud_rate_3M")]
    const BAUD_RATE: u32 = 3_000_000;

    #[cfg(feature = "hardware_flow_control")]
    let hardware_flow_control = true;

    cfg_if::cfg_if! {
        if #[cfg(feature = "uart7")] {
            const PINS: &[(PinSet, Alternate)] = {
                cfg_if::cfg_if! {
                    if #[cfg(feature = "hardware_flow_control")] {
                        &[(
                            Port::E.pin(7).and_pin(8).and_pin(9).and_pin(10),
                            Alternate::AF7
                        )]
                    } else {
                        compile_error!("hardware_flow_control should be enabled");
                    }
                }
            };
            let usart = unsafe { &*device::UART7::ptr() };
            let peripheral = Peripheral::Uart7;
            let pins = PINS;
        } else {
            compile_error!("no usartX/uartX feature specified");
        }
    }

    Usart::turn_on(
        &Sys::from(SYS.get_task_id()),
        usart,
        peripheral,
        pins,
        CLOCK_HZ,
        BAUD_RATE,
        hardware_flow_control,
    )
}

fn claim_uart_rx_buf() -> &'static mut Vec<u8, MAX_PACKET_SIZE> {
    use core::sync::atomic::{AtomicBool, Ordering};

    static mut UART_RX_BUF: Vec<u8, MAX_PACKET_SIZE> = Vec::new();

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::Relaxed) {
        panic!()
    }

    // Safety: unsafe because of references to mutable statics; safe because of
    // the AtomicBool swap above, combined with the lexical scoping of
    // `UART_RX_BUF`, means that this reference can't be aliased by any
    // other reference in the program.
    unsafe { &mut UART_RX_BUF }
}
