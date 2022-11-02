// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use drv_stm32h7_usart as drv_usart;

use drv_gimlet_hf_api::{HfDevSelect, HostFlash};
use drv_gimlet_seq_api::{PowerState, SeqError, Sequencer};
use drv_stm32xx_sys_api as sys_api;
use drv_usart::Usart;
use enum_map::Enum;
use heapless::Vec;
use host_sp_messages::{
    Bsu, DebugReg, DecodeFailureReason, HostToSp, HubpackError, SpToHost,
    Status, MAX_MESSAGE_SIZE,
};
use idol_runtime::{NotificationHandler, RequestError};
use multitimer::{Multitimer, Repeat};
use ringbuf::{ringbuf, ringbuf_entry};
use task_control_plane_agent_api::{ControlPlaneAgent, ControlPlaneAgentError};
use task_host_sp_comms_api::HostSpCommsError;
use userlib::{hl, sys_get_timer, sys_irq_control, task_slot, UnwrapLite};

mod tx_buf;

use tx_buf::TxBuf;

task_slot!(CONTROL_PLANE_AGENT, control_plane_agent);
task_slot!(GIMLET_SEQ, gimlet_seq);
task_slot!(HOST_FLASH, hf);
task_slot!(SYS, sys);

// TODO: When rebooting the host, we need to wait for the relevant power rails
// to decay. We ought to do this properly by monitoring the rails, but for now,
// we'll simply wait a fixed period of time. This time is a WAG - we should
// fix this!
const A2_REBOOT_DELAY: u64 = 5_000;

// How frequently should we try to send 0x00 bytes to the host? This only
// applies if our current tx_buf/rx_buf are empty (i.e., we don't have a real
// response to send, and we haven't yet started to receive a request).
const UART_ZERO_DELAY: u64 = 200;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Trace {
    None,
    Notification { bits: u32 },
    UartTx(u8),
    UartTxFull,
    UartRx(u8),
    UartRxOverrun,
    AckSpStart,
    SetState { now: u64, state: PowerState },
    JefeNotification { now: u64, state: PowerState },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TimerDisposition {
    LeaveRunning,
    Cancel,
}

ringbuf!(Trace, 64, Trace::None);

/// Notification bit for USART IRQ; must match configuration in app.toml.
const USART_IRQ: u32 = 1 << 0;

/// Notification bit for Jefe notifying us of state changes; must match Jefe's
/// `on-state-change` config for us in app.toml.
const JEFE_STATE_CHANGE_IRQ: u32 = 1 << 1;

/// Notification bit for the timer we set for ourselves.
const TIMER_IRQ_BIT: u8 = 2;
const TIMER_IRQ: u32 = 1 << TIMER_IRQ_BIT;

/// Notification bit for control-plane-agent to tell us the phase 2 data the
/// host wants from MGS has arrived.
const CONTROL_PLANE_AGENT_IRQ_BIT: u8 = 3;
const CONTROL_PLANE_AGENT_IRQ: u32 = 1 << CONTROL_PLANE_AGENT_IRQ_BIT;

/// We set the high bit of the sequence number before replying to host requests.
const SEQ_REPLY: u64 = 0x8000_0000_0000_0000;

/// We wrap host/sp messages in corncobs; derive our max packet length from the
/// max unwrapped message length.
const MAX_PACKET_SIZE: usize = corncobs::max_encoded_len(MAX_MESSAGE_SIZE);

#[derive(Copy, Clone, Enum)]
enum Timers {
    /// Timer set when we're waiting in A2 before moving back to A0 for a
    /// reboot.
    WaitingInA2ToReboot,
    /// Timer set when we want to send periodic 0x00 bytes on the uart.
    TxPeriodicZeroByte,
}

#[export_name = "main"]
fn main() -> ! {
    let mut server = ServerImpl::claim_static_resources();

    // Set our restarted status, which interrupts the host to let them know.
    server.set_status_impl(Status::SP_TASK_RESTARTED);
    // XXX For now, we want to default to these options.
    server.set_debug_impl(DebugReg::DEBUG_KMDB | DebugReg::DEBUG_PROM);

    sys_irq_control(USART_IRQ, true);

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch_n(&mut buffer, &mut server);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RebootState {
    // We've instructed the sequencer to transition to A2; we're waiting to see
    // the notification from jefe that that transition has occurred.
    WaitingForA2,
    // We're in our reboot delay (see `A2_REBOOT_DELAY`). When we transition to
    // this state we start our `WaitingInA2ToReboot` timer; when it fires we'll
    // transition to A0.
    WaitingInA2RebootDelay,
}

struct ServerImpl {
    uart: Usart,
    sys: sys_api::Sys,
    timers: Multitimer<Timers>,
    tx_buf: TxBuf,
    rx_buf: &'static mut Vec<u8, MAX_PACKET_SIZE>,
    status: Status,
    debug: DebugReg,
    sequencer: Sequencer,
    hf: HostFlash,
    cp_agent: ControlPlaneAgent,
    reboot_state: Option<RebootState>,
}

impl ServerImpl {
    fn claim_static_resources() -> Self {
        let sys = sys_api::Sys::from(SYS.get_task_id());
        let uart = configure_uart_device(&sys);
        sp_to_sp3_interrupt_enable(&sys);

        let mut timers = Multitimer::new(TIMER_IRQ_BIT);
        timers.set_timer(
            Timers::TxPeriodicZeroByte,
            sys_get_timer().now,
            Some(Repeat::AfterWake(UART_ZERO_DELAY)),
        );

        Self {
            uart,
            sys,
            timers,
            tx_buf: TxBuf::claim_static_resources(),
            rx_buf: claim_uart_rx_buf(),
            status: Status::empty(),
            debug: DebugReg::empty(),
            sequencer: Sequencer::from(GIMLET_SEQ.get_task_id()),
            hf: HostFlash::from(HOST_FLASH.get_task_id()),
            cp_agent: ControlPlaneAgent::from(
                CONTROL_PLANE_AGENT.get_task_id(),
            ),
            reboot_state: None,
        }
    }

    fn set_status_impl(&mut self, status: Status) {
        if status != self.status {
            self.status = status;
            // SP_TO_SP3_INT_L: `INT_L` is "interrupt low", so we assert the pin
            // when we do not have status and deassert it when we do.
            if self.status.is_empty() {
                self.sys.gpio_set(SP_TO_SP3_INT_L).unwrap_lite();
            } else {
                self.sys.gpio_reset(SP_TO_SP3_INT_L).unwrap_lite();
            }
        }
    }

    fn set_debug_impl(&mut self, debug: DebugReg) {
        if debug != self.debug {
            self.debug = debug;
        }
    }

    /// Power off the host (i.e., transition to A2).
    ///
    /// If `reboot` is true and we successfully instruct the sequencer to
    /// transition to A2, we set `self.reboot_state` to
    /// `RebootState::WaitingForA2`. Once we receive the notification from Jefe
    /// that the transition is complete, we'll update that state to
    /// `RebootState::WaitingInA2RebootDelay` and start our timer.
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
    ///    `RebootState::WaitingInA2RebootDelay` and will attempt to move back
    ///    to A0 once we pass that deadline.
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
                        self.timers.set_timer(
                            Timers::WaitingInA2ToReboot,
                            sys_get_timer().now + A2_REBOOT_DELAY,
                            None,
                        );
                        self.reboot_state =
                            Some(RebootState::WaitingInA2RebootDelay);
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
                    self.timers.set_timer(
                        Timers::WaitingInA2ToReboot,
                        now + A2_REBOOT_DELAY,
                        None,
                    );
                    self.reboot_state =
                        Some(RebootState::WaitingInA2RebootDelay);
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

    // State diagram for our uart handler:
    //
    //      Start (main)
    //          │
    //==========│========================================================
    //   ┌──────▼──────────────────────────────────────┐
    // ┌─► Enable repeating Timers::TxPeriodicZeroByte ◄──┐
    // │ └─────────────────────────────────────────────┘  │
    //=│==================================================│==============
    // │  ┌────────────────────┐                          │success
    // │  │   Are we waiting   │     ┌──────────────┐     │
    // │  │to build a response?│   ┌─►try to tx 0x00├─────┘
    // │  └┬────────┬──────────┘   │ └─┬────────────┘
    // │   │no      │yes           │   │
    // │   │    ┌───▼────────┐     │   │TX FIFO full
    // │   │    │Cancel timer│     │ ┌─▼─────────────┐
    // │   │    └────────────┘     │ │Enable TX FIFO ◄────────────┐
    // │   │                       │ │empty interrupt│            │
    // │ ┌─▼────────────────────┐no│ └─┬─────────────┘            │
    // │ │Do we have packet data├──┘   │                          │
    // │ │to tx, or have we rx'd│yes┌──▼────────────────────┐     │
    // │ │   a partial packet?  ├───►Wait for Uart interrupt◄─────┼─┐
    // │ └──────────────────────┘   └─┬─────────────────────┘     │ │
    // │                              │                           │ │
    // │                              │interrupt received         │ │
    // │Timer Interrupt Handler       │                           │ │
    //=│==============================▼===========================│=│====
    // │Uart Interrupt Handler                                    │ │
    // │   ┌─────────────────────────────┐                        │ │
    // │   │Do we have packet data to tx?├───┐                    │ │
    // │   └──────────────┬───────▲──────┘   │yes                 │ │
    // │                  │no     │          │                    │ │
    // │       ┌──────────▼────┐  │  ┌───────▼───────────┐        │ │
    // │       │Disable TX FIFO│  │  │try to tx data byte│        │ │
    // │       │empty interrupt│  │  └─┬──────────▲──┬───┘        │ │
    // │       └──────────┬────┘  │    │success   │  │tx fifo full│ │
    // │                  │       └────┘          │  └────────────┘ │
    // │                  │                       │                 │
    // │         fail ┌───▼──────────────┐◄─────┐ │                 │
    // │         ┌────┤Try to rx one byte│      │ │                 │
    // │         │    └───┬──────────────┘◄──┐  │ │                 │
    // │         │        │success           │  │ │                 │
    // │         │    ┌───▼──────────────┐no │  │ │                 │
    // │         │    │ Is this a packet ├───┘  │ │                 │
    // │         │    │terminator (0x00)?│      │ │                 │
    // │         │    └───┬──────────────┘      │ │                 │
    // │         │        │yes                  │ │                 │
    // │         │      ┌─▼────────────┐ yes    │ │                 │
    // │         │      │Is this packet├────────┘ │                 │
    // │         │      │    empty?    │          │                 │
    // │         │      └─┬────────────┘          │                 │
    // │         │        │no                     │                 │
    // │         │      ┌─▼─────────────┐         │                 │
    // │         │      │Process Message│         │                 │
    // │         │      └─┬─────────────┘         │                 │
    // │         │        │                       │                 │
    // │         │      ┌─▼─────────────┐ yes     │                 │
    // │         │      │ Do we have a  ├─────────┘                 │
    // │         │      │response ready?│                           │
    // │         │      └─────┬─────────┘  ┌──────────────────────┐ │
    // │         │            │ no         │Wait to build         │ │
    // │         │            └────────────►response (notification│ │
    // │         │                         │from another task)    │ │
    // │         │                         └──────────────────────┘ │
    // │        ┌▼────────────────┐                                 │
    // └────────┤  Have we rx'd   ├─────────────────────────────────┘
    //       no │a partial packet?│ yes
    //          └─────────────────┘
    fn handle_usart_notification(&mut self) {
        'tx: loop {
            // Do we have data to transmit? If so, write as much as we can until
            // either the fifo fills (in which case we return before trying to
            // receive more) or we finish flushing.
            while let Some(b) = self.tx_buf.next_byte_to_send() {
                if try_tx_push(&self.uart, b) {
                    self.tx_buf.advance_one_byte();
                } else {
                    // We have more data to send but the TX FIFO is full; enable
                    // the TX FIFO empty interrupt and wait for it.
                    self.timers.clear_timer(Timers::TxPeriodicZeroByte);
                    self.uart.enable_tx_fifo_empty_interrupt();
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
            'rx: while let Some(byte) = self.uart.try_rx_pop() {
                ringbuf_entry!(Trace::UartRx(byte));

                if byte != 0x00 {
                    // This is the end of a packet; buffer it and continue.
                    if self.rx_buf.push(byte).is_err() {
                        // Message overflow - nothing we can do here except
                        // discard data. We'll drop this byte and wait til we
                        // see a 0 to respond, at which point our
                        // deserialization will presumably fail and we'll send
                        // back an error. Should we record that we overflowed
                        // here?
                    }
                    continue 'rx;
                }

                // Host may send extra cobs terminators; skip any empty
                // packets.
                if self.rx_buf.is_empty() {
                    continue 'rx;
                }

                // Process message and set up `self.tx_buf` with our response
                // (or intermediate state if we don't have a response yet).
                self.process_message();
                self.rx_buf.clear();

                // If we have data to send now, immediately loop back to the top
                // and start trying to send it.
                if self.tx_buf.next_byte_to_send().is_some() {
                    continue 'tx;
                }
            }

            // We received everything we could out of the rx fifo and we have
            // nothing to send; we're done.
            //
            // If we haven't receiving anything, set our timer to send out
            // periodic zero bytes. If we have received something, leave the
            // timer clear - we're waiting on more data from the host.
            if self.rx_buf.is_empty() {
                self.timers.set_timer(
                    Timers::TxPeriodicZeroByte,
                    sys_get_timer().now,
                    Some(Repeat::AfterWake(UART_ZERO_DELAY)),
                );
            } else {
                self.timers.clear_timer(Timers::TxPeriodicZeroByte);
            }
            return;
        }
    }

    fn handle_control_plane_agent_notification(&mut self) {
        // If control-plane-agent notified us, presumably it's telling us that
        // the data we asked it to fetch is ready.
        if let Some(phase2) = self.tx_buf.is_waiting_for_phase2_data() {
            // Borrow `cp_agent` to avoid borrowing `self` in the closure below.
            let cp_agent = &self.cp_agent;

            self.tx_buf.encode_response(
                phase2.sequence,
                &SpToHost::Phase2Data,
                |dst| {
                    // Fetch the phase two data directly into `dst` (the buffer
                    // where we're serializing our response), which is maximally
                    // sized for what we can send the host in one packet. It is
                    // almost certainly larger than what control-plane-agent can
                    // fetch in a single UDP packet.
                    match cp_agent.get_host_phase2_data(
                        phase2.hash,
                        phase2.offset,
                        dst,
                    ) {
                        Ok(n) => n,
                        // If we can't get data, all we can do is send the
                        // host a response with no data; it can decide to
                        // retry later.
                        Err(ControlPlaneAgentError::DataUnavailable) => 0,
                    }
                },
            );

            // Call our usart handler, because we now have data to send.
            self.handle_usart_notification();
        }
    }

    // Process the framed packet sitting in `self.rx_buf`. If it warrants a
    // response, we configure `self.tx_buf` appropriate: either populating it
    // with a response if we can come up with that response immediately, or
    // instructing it that we'll fill it in with our response later.
    fn process_message(&mut self) {
        let deframed = match corncobs::decode_in_place(self.rx_buf) {
            Ok(n) => &self.rx_buf[..n],
            Err(_) => {
                self.tx_buf
                    .encode_decode_failure_reason(DecodeFailureReason::Cobs);
                return;
            }
        };

        let (header, request, _data) =
            match host_sp_messages::deserialize::<HostToSp>(deframed) {
                Ok((header, request, data)) => (header, request, data),
                Err(HubpackError::Custom) => {
                    self.tx_buf
                        .encode_decode_failure_reason(DecodeFailureReason::Crc);
                    return;
                }
                Err(_) => {
                    self.tx_buf.encode_decode_failure_reason(
                        DecodeFailureReason::Deserialize,
                    );
                    return;
                }
            };

        if header.magic != host_sp_messages::MAGIC {
            self.tx_buf.encode_decode_failure_reason(
                DecodeFailureReason::MagicMismatch,
            );
            return;
        }

        if header.version != host_sp_messages::version::V1 {
            self.tx_buf.encode_decode_failure_reason(
                DecodeFailureReason::VersionMismatch,
            );
            return;
        }

        if header.sequence & SEQ_REPLY != 0 {
            self.tx_buf.encode_decode_failure_reason(
                DecodeFailureReason::SequenceInvalid,
            );
            return;
        }

        // We defer any actions until after we've serialized our response to
        // avoid borrow checker issues with calling methods on `self`.
        let mut action = None;
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
                // Per RFD 241, the phase 1 device (which we can read via
                // `hf`) is tightly bound to the BSU, so we can map flash0 to
                // BSU A and flash1 to BSU B.
                //
                // What should we do if we fail to get the device from the host
                // flash task? That should only happen if `hf` is unable to
                // respond to us at all, which makes it seem unlikely that the
                // host could even be up. We'll default to returning Bsu::A.
                //
                // Minor TODO: Attempting to get the BSU on a gimletlet will
                // hang, because the host-flash task hangs indefinitely. We
                // could replace gimlet-hf-server with a fake on gimletlet if
                // that becomes onerous.
                let bsu = match self.hf.get_dev() {
                    Ok(HfDevSelect::Flash0) | Err(_) => Bsu::A,
                    Ok(HfDevSelect::Flash1) => Bsu::B,
                };
                Some(SpToHost::BootStorageUnit(bsu))
            }
            HostToSp::GetIdentity => {
                // TODO how do we get our real identity?
                Some(SpToHost::Identity {
                    model: *b"913-0000019",
                    revision: 2,
                    serial: *b"OXE99990000",
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
            HostToSp::GetStatus => Some(SpToHost::Status {
                status: self.status,
                debug: self.debug,
            }),
            HostToSp::AckSpStart => {
                ringbuf_entry!(Trace::AckSpStart);
                action =
                    Some(Action::ClearStatusBits(Status::SP_TASK_RESTARTED));
                Some(SpToHost::Ack)
            }
            HostToSp::GetAlert => {
                // TODO define alerts
                Some(SpToHost::Alert { action: 0 })
            }
            HostToSp::RotRequest => {
                // TODO forward request to RoT
                Some(SpToHost::RotResponse)
            }
            HostToSp::RotAddHostMeasurements => {
                // TODO forward request to RoT
                Some(SpToHost::Ack)
            }
            HostToSp::GetPhase2Data { hash, offset } => {
                // We don't have a response to transmit now, but need to avoid
                // sending periodic 0s until we do have a response. Instruct
                // `tx_buf` that we're waiting for the host phase2 data to show
                // up.
                self.tx_buf.set_waiting_for_phase2_data(
                    header.sequence,
                    hash,
                    offset,
                );

                // Ask control-plane-agent to fetch this data for us; it will
                // notify us when it arrives.
                self.cp_agent
                    .fetch_host_phase2_data(
                        hash,
                        offset,
                        CONTROL_PLANE_AGENT_IRQ_BIT,
                    )
                    .unwrap_lite();
                None
            }
        };

        if let Some(response) = response {
            // If we have a response immediately, we have no extra data to
            // pack into the packet, hence the 0-returning closure.
            self.tx_buf
                .encode_response(header.sequence, &response, |_| 0);
        }

        // Now that all buffer borrowing is done, we can borrow `self` mutably
        // again to perform any necessary action.
        if let Some(action) = action {
            match action {
                Action::RebootHost => self.power_off_host(true),
                Action::PowerOffHost => self.power_off_host(false),
                Action::ClearStatusBits(to_clear) => {
                    self.set_status_impl(self.status.difference(to_clear))
                }
            }
        }
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        USART_IRQ | JEFE_STATE_CHANGE_IRQ | TIMER_IRQ | CONTROL_PLANE_AGENT_IRQ
    }

    fn handle_notification(&mut self, bits: u32) {
        ringbuf_entry!(Trace::Notification { bits });

        if bits & USART_IRQ != 0 {
            self.handle_usart_notification();
            sys_irq_control(USART_IRQ, true);
        }

        if bits & JEFE_STATE_CHANGE_IRQ != 0 {
            self.handle_jefe_notification(
                self.sequencer.get_state().unwrap_lite(),
            );
        }

        if bits & CONTROL_PLANE_AGENT_IRQ != 0 {
            self.handle_control_plane_agent_notification();
        }

        // We may want to clear our TX periodic zero byte timer (if the TX FIFO
        // is full), but we can't modify the timers while iterating over them.
        // We'll record whether or not we want to clear the timer in this
        // variable, then actually clear it (if needed) after the loop over the
        // fired timers.
        self.timers.handle_notification(bits);
        let mut tx_timer_disposition = TimerDisposition::LeaveRunning;
        for t in self.timers.iter_fired() {
            match t {
                Timers::WaitingInA2ToReboot => {
                    handle_reboot_waiting_in_a2_timer(
                        &self.sequencer,
                        &mut self.reboot_state,
                    );
                }
                Timers::TxPeriodicZeroByte => {
                    tx_timer_disposition = handle_tx_periodic_zero_byte_timer(
                        &self.uart,
                        &self.tx_buf,
                        &self.rx_buf,
                    );
                }
            }
        }

        match tx_timer_disposition {
            TimerDisposition::LeaveRunning => (),
            TimerDisposition::Cancel => {
                self.timers.clear_timer(Timers::TxPeriodicZeroByte);
            }
        }
    }
}

// This is conceptually a method on `ServerImpl`, but it takes references to
// several of its fields instead of `self` to avoid borrow checker issues.
fn handle_reboot_waiting_in_a2_timer(
    sequencer: &Sequencer,
    reboot_state: &mut Option<RebootState>,
) {
    // If we're past the deadline for transitioning to A0, attempt to do so.
    if let Some(RebootState::WaitingInA2RebootDelay) = reboot_state {
        // The only way our reboot state gets set to
        // `WaitingInA2RebootDelay` is if we believe we were currently in
        // A2. Attempt to transition to A0, which can only fail if we're no
        // longer in A2. In either case (we successfully started the
        // transition or we're no longer in A2 due to some external cause),
        // we've done what we can to reboot, so clear out `reboot_state`.
        ringbuf_entry!(Trace::SetState {
            now: sys_get_timer().now,
            state: PowerState::A0,
        });
        _ = sequencer.set_state(PowerState::A0);
        *reboot_state = None;
    }
}

// This is conceptually a method on `ServerImpl`, but it takes references to
// several of its fields instead of `self` to avoid borrow checker issues.
fn handle_tx_periodic_zero_byte_timer(
    uart: &Usart,
    tx_buf: &TxBuf,
    rx_buf: &Vec<u8, MAX_PACKET_SIZE>,
) -> TimerDisposition {
    if tx_buf.should_send_periodic_zero_bytes() && rx_buf.is_empty() {
        // We don't have a real packet we're sending and we haven't
        // started receiving a request from the host; try to send a
        // 0x00 terminator. If we can, reset the deadline to send
        // another one after `UART_ZERO_DELAY`; if we can't, disable
        // our timer and wait for a uart interrupt instead.
        if try_tx_push(uart, 0) {
            TimerDisposition::LeaveRunning
        } else {
            // If we have no real packet data but we've filled the
            // TX FIFO (presumably with zeroes from this deadline
            // firing $TX_FIFO_DEPTH times, although possibly
            // because we just finished sending a real packet),
            // we're waiting on the host to read the data out of our
            // TX FIFO: we don't need to push any more zeroes until
            // the host has read everything out of our FIFO.
            // Therefore, enable the TX FIFO empty interrupt and
            // stop waking up on a timer; when the uart interrupt
            // fires (either due to the host sending us data or
            // draining the TX FIFO), we'll reset the timer then if
            // needed.
            uart.enable_tx_fifo_empty_interrupt();
            TimerDisposition::Cancel
        }
    } else {
        // We're either sending or receiving a real packet; disable
        // our "sending 0s" timer until that finishes. The
        // appropriate uart interrupt(s) are already enabled.
        TimerDisposition::Cancel
    }
}

impl idl::InOrderHostSpCommsImpl for ServerImpl {
    fn set_status(
        &mut self,
        _msg: &userlib::RecvMessage,
        status: u64,
    ) -> Result<(), RequestError<HostSpCommsError>> {
        let status =
            Status::from_bits(status).ok_or(HostSpCommsError::InvalidStatus)?;

        self.set_status_impl(status);

        Ok(())
    }

    fn get_status(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<Status, RequestError<HostSpCommsError>> {
        Ok(self.status)
    }

    fn set_debug(
        &mut self,
        _msg: &userlib::RecvMessage,
        debug: u64,
    ) -> Result<(), RequestError<HostSpCommsError>> {
        let debug =
            DebugReg::from_bits(debug).ok_or(HostSpCommsError::InvalidDebug)?;

        self.set_debug_impl(debug);

        Ok(())
    }

    fn get_debug(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<DebugReg, RequestError<HostSpCommsError>> {
        Ok(self.debug)
    }
}

// Borrow checker workaround; list of actions we perform in response to a host
// request _after_ we're done borrowing any message buffers.
enum Action {
    RebootHost,
    PowerOffHost,
    ClearStatusBits(Status),
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

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
fn configure_uart_device(sys: &sys_api::Sys) -> Usart {
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
        sys,
        usart,
        peripheral,
        pins,
        CLOCK_HZ,
        BAUD_RATE,
        hardware_flow_control,
    )
}

cfg_if::cfg_if! {
    if #[cfg(any(
        target_board = "gimlet-a",
        target_board = "gimlet-b",
        target_board = "gimlet-c",
    ))] {
        const SP_TO_SP3_INT_L: sys_api::PinSet = sys_api::Port::I.pin(7);
    } else if #[cfg(target_board = "gimletlet-2")] {
        // gimletlet doesn't have an SP3 to interrupt, but we can wire up an LED
        // to one of the exposed E2-E6 pins to see it visually.
        const SP_TO_SP3_INT_L: sys_api::PinSet = sys_api::Port::E.pin(2);
    } else {
        compile_error!("unsupported target board");
    }
}

fn sp_to_sp3_interrupt_enable(sys: &sys_api::Sys) {
    sys.gpio_set(SP_TO_SP3_INT_L).unwrap();

    sys.gpio_configure_output(
        SP_TO_SP3_INT_L,
        sys_api::OutputType::OpenDrain,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    )
    .unwrap();
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

mod idl {
    use task_host_sp_comms_api::{DebugReg, HostSpCommsError, Status};
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
