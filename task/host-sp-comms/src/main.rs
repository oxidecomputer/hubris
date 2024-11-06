// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

#[cfg(any(feature = "stm32h743", feature = "stm32h753"))]
use drv_stm32h7_usart as drv_usart;

use attest_data::messages::{
    HostToRotCommand, RecvSprotError as AttestDataSprotError, RotToHost,
    MAX_DATA_LEN,
};
use drv_cpu_seq_api::{PowerState, SeqError, Sequencer};
use drv_hf_api::{HfDevSelect, HfMuxState, HostFlash};
use drv_sprot_api::SpRot;
use drv_stm32xx_sys_api as sys_api;
use drv_usart::Usart;
use enum_map::Enum;
use heapless::Vec;
use host_sp_messages::{
    Bsu, DecodeFailureReason, Header, HostToSp, Key, KeyLookupResult,
    KeySetResult, SpToHost, Status, MAX_MESSAGE_SIZE,
    MIN_SP_TO_HOST_FILL_DATA_LEN,
};
use hubpack::SerializedSize;
use idol_runtime::{NotificationHandler, RequestError};
use multitimer::{Multitimer, Repeat};
use ringbuf::{counted_ringbuf, ringbuf_entry};
use static_assertions::const_assert;
use static_cell::ClaimOnceCell;
use task_control_plane_agent_api::{
    ControlPlaneAgent, MAX_INSTALLINATOR_IMAGE_ID_LEN,
};
use task_host_sp_comms_api::HostSpCommsError;
use task_net_api::Net;
use task_packrat_api::Packrat;
use userlib::{
    hl, sys_get_timer, sys_irq_control, task_slot, FromPrimitive, UnwrapLite,
};

mod inventory;
use inventory::INVENTORY_API_VERSION;

#[cfg_attr(
    any(
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
    ),
    path = "bsp/gimlet_bcde.rs"
)]
#[cfg_attr(target_board = "gimletlet-2", path = "bsp/gimletlet.rs")]
#[cfg_attr(target_board = "grapefruit", path = "bsp/grapefruit.rs")]
mod bsp;

mod tx_buf;
use tx_buf::TxBuf;

task_slot!(CONTROL_PLANE_AGENT, control_plane_agent);
task_slot!(CPU_SEQ, cpu_seq);
task_slot!(HOST_FLASH, hf);
task_slot!(PACKRAT, packrat);
task_slot!(NET, net);
task_slot!(SYS, sys);
task_slot!(SPROT, sprot);

// TODO: When rebooting the host, we need to wait for the relevant power rails
// to decay. We ought to do this properly by monitoring the rails, but for now,
// we'll simply wait a fixed period of time. This time is a WAG - we should
// fix this!
const A2_REBOOT_DELAY: u64 = 5_000;

// How frequently should we try to send 0x00 bytes to the host? This only
// applies if our current tx_buf/rx_buf are empty (i.e., we don't have a real
// response to send, and we haven't yet started to receive a request).
const UART_ZERO_DELAY: u64 = 200;

// How long of a host panic / boot fail message are we willing to keep?
const MAX_HOST_FAIL_MESSAGE_LEN: usize = 4096;

// How many MAC addresses should we report to the host? Per RFD 320, a gimlet
// currently needs 5 total:
//
// * 2 for the T6
// * 2 for the management network (already claimed by `net`)
// * 1 for the bootstrap network prefix
//
// Subtracting out the 2 already claimed by `net`, we will only give the host 3
// MAC addresses, even if `net` tells us more are available. In the future, if
// we need to increase the number given to the host, that's easy to do here; if
// we need to increase the number used by the SP, ideally `net` will take care
// of that for us.
const NUM_HOST_MAC_ADDRESSES: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    UartRx(u8),
    UartRxOverrun,
    ParseError(#[count(children)] DecodeFailureReason),
    SetState {
        now: u64,
        #[count(children)]
        state: PowerState,
    },
    HfMux {
        now: u64,
        state: Option<HfMuxState>,
    },
    JefeNotification {
        now: u64,
        #[count(children)]
        state: PowerState,
    },
    OutOfSyncRequest,
    OutOfSyncRxNoise,
    Request {
        now: u64,
        sequence: u64,
        #[count(children)]
        message: HostToSp,
    },
    ResponseBufferReset {
        now: u64,
    },
    Response {
        now: u64,
        sequence: u64,
        #[count(children)]
        message: SpToHost,
    },
}

counted_ringbuf!(Trace, 50, Trace::None);

#[derive(Debug, Clone, Copy, PartialEq)]
enum TimerDisposition {
    LeaveRunning,
    Cancel,
}

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

    sys_irq_control(notifications::USART_IRQ_MASK, true);

    let mut buffer = [0; idl::INCOMING_SIZE];
    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
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

const MAX_ETC_SYSTEM_LEN: usize = 256;
const MAX_DTRACE_CONF_LEN: usize = 4096;

// Storage we set aside for any messages where the host wants us to remember
// data for later read back (either by the host itself or by the control plane
// via MGS).
struct HostKeyValueStorage {
    last_boot_fail_reason: u8,
    last_boot_fail: &'static mut [u8; MAX_HOST_FAIL_MESSAGE_LEN],
    last_panic: &'static mut [u8; MAX_HOST_FAIL_MESSAGE_LEN],
    etc_system: &'static mut [u8; MAX_ETC_SYSTEM_LEN],
    etc_system_len: usize,
    dtrace_conf: &'static mut [u8; MAX_DTRACE_CONF_LEN],
    dtrace_conf_len: usize,
}

impl HostKeyValueStorage {
    fn key_set(&mut self, key: u8, data: &[u8]) -> KeySetResult {
        let Some(key) = Key::from_u8(key) else {
            return KeySetResult::InvalidKey;
        };

        let (buf, buf_len) = match key {
            // Some keys should not be set by the host:
            //
            // * `Ping` always returns PONG
            // * InstallinatorImageId is set via MGS
            // * InventorySize always returns our static inventory size
            Key::Ping | Key::InstallinatorImageId | Key::InventorySize => {
                return KeySetResult::ReadOnlyKey;
            }
            Key::EtcSystem => {
                (self.etc_system.as_mut_slice(), &mut self.etc_system_len)
            }
            Key::DtraceConf => {
                (self.dtrace_conf.as_mut_slice(), &mut self.dtrace_conf_len)
            }
        };

        if data.len() > buf.len() {
            KeySetResult::DataTooLong
        } else {
            buf[..data.len()].copy_from_slice(data);
            *buf_len = data.len();
            KeySetResult::Ok
        }
    }
}

struct ServerImpl {
    uart: Usart,
    sys: sys_api::Sys,
    timers: Multitimer<Timers>,
    tx_buf: TxBuf,
    rx_buf: &'static mut Vec<u8, MAX_PACKET_SIZE>,
    status: Status,
    sequencer: Sequencer,
    hf: HostFlash,
    net: Net,
    cp_agent: ControlPlaneAgent,
    packrat: Packrat,
    sprot: SpRot,
    reboot_state: Option<RebootState>,
    host_kv_storage: HostKeyValueStorage,
    hf_mux_state: Option<HfMuxState>,
}

impl ServerImpl {
    fn claim_static_resources() -> Self {
        let sys = sys_api::Sys::from(SYS.get_task_id());
        let uart = configure_uart_device(&sys);
        sp_to_sp3_interrupt_enable(&sys);

        let mut timers = Multitimer::new(notifications::MULTITIMER_BIT);
        timers.set_timer(
            Timers::TxPeriodicZeroByte,
            sys_get_timer().now,
            Some(Repeat::AfterWake(UART_ZERO_DELAY)),
        );

        struct Bufs {
            tx_buf: tx_buf::StaticBufs,
            rx_buf: Vec<u8, MAX_PACKET_SIZE>,
            last_boot_fail: [u8; MAX_HOST_FAIL_MESSAGE_LEN],
            last_panic: [u8; MAX_HOST_FAIL_MESSAGE_LEN],
            etc_system: [u8; MAX_ETC_SYSTEM_LEN],
            dtrace_conf: [u8; MAX_DTRACE_CONF_LEN],
        }
        let Bufs {
            ref mut tx_buf,
            ref mut rx_buf,
            ref mut last_boot_fail,
            ref mut last_panic,
            ref mut etc_system,
            ref mut dtrace_conf,
        } = {
            static BUFS: ClaimOnceCell<Bufs> = ClaimOnceCell::new(Bufs {
                tx_buf: tx_buf::StaticBufs::new(),
                rx_buf: Vec::new(),
                last_boot_fail: [0; MAX_HOST_FAIL_MESSAGE_LEN],
                last_panic: [0; MAX_HOST_FAIL_MESSAGE_LEN],
                etc_system: [0; MAX_ETC_SYSTEM_LEN],
                dtrace_conf: [0; MAX_DTRACE_CONF_LEN],
            });
            BUFS.claim()
        };
        Self {
            uart,
            sys,
            timers,
            tx_buf: tx_buf::TxBuf::new(tx_buf),
            rx_buf,
            status: Status::empty(),
            sequencer: Sequencer::from(CPU_SEQ.get_task_id()),
            hf: HostFlash::from(HOST_FLASH.get_task_id()),
            net: Net::from(NET.get_task_id()),
            cp_agent: ControlPlaneAgent::from(
                CONTROL_PLANE_AGENT.get_task_id(),
            ),
            packrat: Packrat::from(PACKRAT.get_task_id()),
            sprot: SpRot::from(SPROT.get_task_id()),
            reboot_state: None,
            host_kv_storage: HostKeyValueStorage {
                last_boot_fail_reason: 0,
                last_boot_fail,
                last_panic,
                etc_system,
                etc_system_len: 0,
                dtrace_conf,
                dtrace_conf_len: 0,
            },
            hf_mux_state: None,
        }
    }

    fn set_status_impl(&mut self, status: Status) {
        if status != self.status {
            self.status = status;
            // SP_TO_HOST_CPU_INT_L: `INT_L` is "interrupt low", so we assert the pin
            // when we do not have status and deassert it when we do.
            if self.status.is_empty() {
                self.sys.gpio_set(SP_TO_HOST_CPU_INT_L);
            } else {
                self.sys.gpio_reset(SP_TO_HOST_CPU_INT_L);
            }
        }
    }

    fn update_hf_mux_state(&mut self) {
        self.hf_mux_state = self.hf.get_mux().ok();
        ringbuf_entry!(Trace::HfMux {
            now: sys_get_timer().now,
            state: self.hf_mux_state,
        });
    }

    fn set_hf_mux_to_sp(&mut self) {
        if self.hf_mux_state != Some(HfMuxState::SP) {
            // This can only fail if the `hf` task panics, in which case the
            // MUX will be set to the SP when it restarts.
            let _ = self.hf.set_mux(HfMuxState::SP);
            self.update_hf_mux_state();
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
            match self.sequencer.get_state() {
                // If we're in A0, we should've been able to transition to A2;
                // just repeat our loop and try again.
                PowerState::A0
                | PowerState::A0PlusHP
                | PowerState::A0Thermtrip
                | PowerState::A0Reset => continue,

                // If we're already in A2 somehow, we're done.
                PowerState::A2 | PowerState::A2PlusFans => {
                    if reboot {
                        // Somehow we're already in A2 when the host wanted to
                        // reboot; set our reboot timer.
                        //
                        // Using saturating add here because it's cheaper than
                        // potentially panicking, and timestamps won't saturate
                        // for 584 million years.
                        self.timers.set_timer(
                            Timers::WaitingInA2ToReboot,
                            sys_get_timer().now.saturating_add(A2_REBOOT_DELAY),
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
        self.update_hf_mux_state();
        // If we're rebooting and jefe has notified us that we're now in A2,
        // move to A0. Otherwise, ignore this notification.
        match state {
            PowerState::A2 | PowerState::A2PlusFans => {
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
            PowerState::A0Reset => {
                // We have spontaneously reset.  We are in A0 (and indeed,
                // by time we get this, the ABL is presumably running), but
                // we cannot let the SoC simply reset because the true state
                // of hidden cores is unknown:  explicitly bounce to A2
                // as if the host had requested it.
                self.power_off_host(true);
            }

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
    // │   │    └────────────┘     │ │Enable TX FIFO ◄────────────────┐
    // │   │                       │ │empty interrupt│                │
    // │ ┌─▼────────────────────┐no│ └─┬─────────────┘                │
    // │ │Do we have packet data├──┘   │                              │
    // │ │to tx, or have we rx'd│yes┌──▼────────────────────┐         │
    // │ │   a partial packet?  ├───►Wait for Uart interrupt◄───── ─┐ │
    // │ └──────────────────────┘   └─┬─────────────────────┘       │ │
    // │                              │                             │ │
    // │                              │interrupt received           │ │
    // │Timer Interrupt Handler       │                             │ │
    //=│==============================▼=============================│=│==
    // │Uart Interrupt Handler                                      │ │
    // │   ┌─────────────────────────────┐                          │ │
    // │   │Do we have packet data to tx?├───┐                      │ │
    // │   └──────────────┬───────▲──────┘   │yes                   │ │
    // │                  │no     │          │                      │ │
    // │       ┌──────────▼────┐  │  ┌───────▼───────────┐          │ │
    // │       │Disable TX FIFO│  │  │try to tx data byte│          │ │
    // │       │empty interrupt│  │  └─┬──────────▲──┬───┘          │ │
    // │       └──────────┬────┘  │    │success   │  │tx fifo full  │ │
    // │                  │       └────┘          │  │              │ │
    // │                  │                       │  │              │ │
    // │         fail ┌───▼──────────────┐◄─────┐ │ ┌▼────────────┐ │ │
    // │         ┌────┤Try to rx one byte│      │ │ │Do we have an│ │ │no
    // │         │    └───┬──────────────┘◄──┐  │ │ │out-of-order ├─┼─┘
    // │         │        │success           │  │ │ │request from │ │
    // │         │    ┌───▼──────────────┐no │  │ │ │the host?    │ │
    // │         │    │ Is this a packet ├───┘  │ │ └─┬───────────┘ │
    // │         │    │terminator (0x00)?│      │ │   │             │
    // │         │    └───┬──────────────┘      │ │ ┌─▼─────────┐   │
    // │         │        │yes                  │ │ │Discard any│   │
    // │         │      ┌─▼────────────┐ yes    │ │ │remaining  │   │
    // │         │      │Is this packet├────────┘ │ │tx data    │   │
    // │         │      │    empty?    │          │ └──┬────────┘   │
    // │         │      └─┬────────────┘          │    │            │
    // │         │        │no                     │    │            │
    // │         │      ┌─▼─────────────┐         │    │            │
    // │         │      │Process Message◄─────────┼────┘            │
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
            // Clear any RX overrun errors. If we hit this, we will likely fail
            // to decode the next message from the host, which will cause us to
            // send a `DecodeFailure` response.
            if self.uart.check_and_clear_rx_overrun() {
                ringbuf_entry!(Trace::UartRxOverrun);
            }

            let mut processed_out_of_sync_message = false;

            // Do we have data to transmit? If so, write as much as we can until
            // either the fifo fills (in which case we return before trying to
            // receive more) or we finish flushing.
            while let Some(b) = self.tx_buf.next_byte_to_send() {
                if self.uart.try_tx_push(b) {
                    self.tx_buf.advance_one_byte();
                } else if self.uart_rx_until_maybe_packet() {
                    // We still have data to send, but the host has sent us a
                    // packet! First, we'll try to decode it: if that succeeds,
                    // something has gone wrong (from our point of view the host
                    // has broken protocol). We'll deal with this by:
                    //
                    // 1. Discarding any remaining data we have from the old
                    //    response.
                    // 2. Sending a 0x00 terminator so the host can detect the
                    //    end of that old (partial) packet.
                    // 3. Handling the new request.
                    //
                    // 1 and 2 are covered by calling `tx_buf.reset()`, which
                    // `process_message` does at our request only if the
                    // packet decodes successfully. If the packet does not
                    // decode successfully, we discard it and assume it was line
                    // noise.
                    match self.process_message(true) {
                        Ok(()) => {
                            processed_out_of_sync_message = true;
                            ringbuf_entry!(Trace::OutOfSyncRequest);
                        }
                        Err(_) => {
                            ringbuf_entry!(Trace::OutOfSyncRxNoise);
                        }
                    }
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

            // It's possible (but unlikely) we've already received a message in
            // this loop iteration. If we have, skip trying to read a request
            // here and move on to either looping back to start sending the
            // response or setting up timers for future interrupts.
            if !processed_out_of_sync_message
                && self.uart_rx_until_maybe_packet()
            {
                // We received a packet; handle it.
                if let Err(reason) = self.process_message(false) {
                    self.tx_buf.encode_decode_failure_reason(reason);
                }
            }

            // If we have data to send now, immediately loop back to the
            // top and start trying to send it.
            if self.tx_buf.next_byte_to_send().is_some() {
                continue 'tx;
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

    fn uart_rx_until_maybe_packet(&mut self) -> bool {
        while let Some(byte) = self.uart.try_rx_pop() {
            ringbuf_entry!(Trace::UartRx(byte));
            if byte == 0x00 {
                // COBS terminator; did we get any data?
                if self.rx_buf.is_empty() {
                    continue;
                } else {
                    return true;
                }
            }

            // Not a COBS terminator; buffer it.
            if self.rx_buf.push(byte).is_err() {
                // Message overflow - nothing we can do here except
                // discard data. We'll drop this byte and wait til we
                // see a 0 to respond, at which point our
                // deserialization will presumably fail and we'll send
                // back an error. Should we record that we overflowed
                // here?
            }
        }

        false
    }

    fn handle_control_plane_agent_notification(&mut self) {
        // If control-plane-agent notified us, presumably it's telling us that
        // the data we asked it to fetch is ready.
        if let Some(phase2) = self.tx_buf.get_waiting_for_phase2_data() {
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
                    //
                    // If we can't get data, all we can do is send the host a
                    // response with no data; it can decide to retry later.
                    cp_agent
                        .get_host_phase2_data(phase2.hash, phase2.offset, dst)
                        .unwrap_or(0)
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
    //
    // If `reset_tx_buf` is true AND we successfully decode a packet, we will
    // call `self.tx_buf.reset()` prior to populating it with a response. This
    // should only be set to true if we're being called in an "out of sync"
    // path; see the comments in `handle_usart_notification()` where we check
    // for an incoming request while we're still trying to send a previous
    // response.
    //
    // This method always (i.e., on success or failure) clears `rx_buf` before
    // returning to prepare for the next packet.
    fn process_message(
        &mut self,
        reset_tx_buf: bool,
    ) -> Result<(), DecodeFailureReason> {
        let (header, request, data) = match parse_received_message(self.rx_buf)
        {
            Ok((header, request, data)) => (header, request, data),
            Err(err) => {
                ringbuf_entry!(Trace::ParseError(err));
                self.rx_buf.clear();
                return Err(err);
            }
        };
        ringbuf_entry!(Trace::Request {
            now: sys_get_timer().now,
            sequence: header.sequence,
            message: request,
        });

        // Reset tx_buf if our caller wanted us to in response to a valid
        // packet.
        if reset_tx_buf {
            self.tx_buf.reset();
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
                // gimlet-seq populates packrat with our identity from VPD prior
                // to transitioning to A2; if the host has requested that
                // identity, we're already in A0 and therefore don't have to
                // wait for packrat. If `get_identity()` fails, it means the
                // sequencer failed to read our VPD; all we can really do is
                // send the host a null (default) identity.
                let identity = self.packrat.get_identity().unwrap_or_default();
                Some(SpToHost::Identity(identity.into()))
            }
            HostToSp::GetMacAddresses => {
                let block = self.net.get_spare_mac_addresses();
                let response = if block.count.get() > 0 {
                    let count =
                        u16::min(block.count.get(), NUM_HOST_MAC_ADDRESSES);
                    SpToHost::MacAddresses {
                        base: block.base_mac,
                        count,
                        stride: block.stride,
                    }
                } else {
                    SpToHost::MacAddresses {
                        base: [0; 6],
                        count: 0,
                        stride: 0,
                    }
                };
                Some(response)
            }
            HostToSp::HostBootFailure { reason } => {
                // TODO forward to MGS
                //
                // For now, copy it into a static var we can pull out via
                // `humility host boot-fail`.
                let n = usize::min(
                    data.len(),
                    self.host_kv_storage.last_boot_fail.len(),
                );
                self.host_kv_storage.last_boot_fail[..n]
                    .copy_from_slice(&data[..n]);
                for b in &mut self.host_kv_storage.last_boot_fail[n..] {
                    *b = 0;
                }
                self.host_kv_storage.last_boot_fail_reason = reason;
                Some(SpToHost::Ack)
            }
            HostToSp::HostPanic => {
                // TODO forward to MGS
                //
                // For now, copy it into a static var we can pull out via
                // `humility host last-panic`.
                let n = usize::min(
                    data.len(),
                    self.host_kv_storage.last_panic.len(),
                );
                self.host_kv_storage.last_panic[..n]
                    .copy_from_slice(&data[..n]);
                for b in &mut self.host_kv_storage.last_panic[n..] {
                    *b = 0;
                }
                Some(SpToHost::Ack)
            }
            HostToSp::GetStatus => {
                // This status request is the first IPCC command that the OS
                // kernel sends once it has started. When we receive it, we
                // know that we're far enough along that the host no longer
                // needs access to the flash.
                // Set the mux back to the SP to remove the host's access,
                // which has the added benefit of enabling host flash updates
                // while the host OS is running.
                action = Some(Action::HfMuxToSP);

                Some(SpToHost::Status {
                    status: self.status,
                    startup: self.packrat.get_next_boot_host_startup_options(),
                })
            }
            HostToSp::AckSpStart => {
                action =
                    Some(Action::ClearStatusBits(Status::SP_TASK_RESTARTED));
                Some(SpToHost::Ack)
            }
            HostToSp::GetAlert => {
                // TODO define alerts
                Some(SpToHost::Alert { action: 0 })
            }
            HostToSp::RotRequest => {
                match attest_data::messages::parse_message(data) {
                    Ok((command, data)) => {
                        let n = usize::min(MAX_DATA_LEN, data.len());

                        let mut data_buf: [u8; MAX_DATA_LEN] =
                            [0; MAX_DATA_LEN];
                        if n > 0 {
                            data_buf[..n].copy_from_slice(&data[..n]);
                        }

                        self.handle_sprot(header.sequence, command, &data_buf);
                    }
                    Err(e) => {
                        self.tx_buf.try_encode_response(
                            header.sequence,
                            &SpToHost::RotResponse,
                            |buf| {
                                attest_data::messages::serialize(
                                    buf,
                                    &RotToHost::HostToRotError(e),
                                    |_| 0,
                                )
                                .map_err(
                                    |err| SpToHost::DecodeFailure(err.into()),
                                )
                            },
                        );
                    }
                }
                None
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
                        notifications::CONTROL_PLANE_AGENT_BIT,
                    )
                    .unwrap_lite();
                None
            }
            HostToSp::KeyLookup {
                key,
                max_response_len,
            } => match self.perform_key_lookup(
                header.sequence,
                key,
                usize::from(max_response_len),
            ) {
                Ok(()) => {
                    // perform_key_lookup() calls encodes the response directly
                    // when it succeeds, so we have nothing else to do.
                    None
                }
                Err(err) => Some(SpToHost::KeyLookupResult(err)),
            },
            HostToSp::KeySet { key } => Some(SpToHost::KeySetResult(
                self.host_kv_storage.key_set(key, data),
            )),
            HostToSp::GetInventoryData { index } => {
                match self.perform_inventory_lookup(header.sequence, index) {
                    Ok(()) => None,
                    Err(err) => Some(SpToHost::InventoryData {
                        result: err,
                        name: [0; 32],
                    }),
                }
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
                Action::HfMuxToSP => self.set_hf_mux_to_sp(),
            }
        }

        // We've processed the message sitting in rx_buf; clear it.
        self.rx_buf.clear();

        Ok(())
    }

    fn handle_sprot(
        &mut self,
        sequence: u64,
        command: HostToRotCommand,
        data: &[u8],
    ) {
        // All the checks returning CommsBufTooSmall are most likely overkill
        // but returning an error is much nicer vs a task crash
        const SPROT_TRANSFER_SIZE: usize = 512;
        match command {
            HostToRotCommand::GetCertificates => {
                let f = |buf: &mut [u8]| {
                    let mut cnt = 0;
                    let cert_chain_len = self
                        .sprot
                        .cert_chain_len()
                        .map_err(AttestDataSprotError::from)?;
                    let mut buf_idx = 0;
                    for index in 0..cert_chain_len {
                        let cert_len = self
                            .sprot
                            .cert_len(index)
                            .map_err(AttestDataSprotError::from)?;
                        for o in (0..cert_len).step_by(SPROT_TRANSFER_SIZE) {
                            let idx: usize = buf_idx + o as usize;
                            let end = usize::min(
                                SPROT_TRANSFER_SIZE,
                                (cert_len - o) as usize,
                            );
                            if idx + end >= buf.len() {
                                return Err(
                                    AttestDataSprotError::CommsBufTooSmall
                                        .into(),
                                );
                            }
                            self.sprot
                                .cert(index, o, &mut buf[idx..idx + end])
                                .map_err(AttestDataSprotError::from)?;
                        }
                        cnt += cert_len;
                        buf_idx += cert_len as usize;
                    }
                    Ok(cnt as usize)
                };
                self.tx_buf.try_encode_response(
                    sequence,
                    &SpToHost::RotResponse,
                    |buf| {
                        attest_data::messages::try_serialize(
                            buf,
                            &RotToHost::RotCertificates,
                            f,
                        )
                        .map_err(|e| SpToHost::DecodeFailure(e.into()))
                    },
                );
            }
            HostToRotCommand::GetMeasurementLog => {
                let f = |buf: &mut [u8]| {
                    let measurement_len = self
                        .sprot
                        .log_len()
                        .map_err(AttestDataSprotError::from)?;
                    for o in (0..measurement_len).step_by(SPROT_TRANSFER_SIZE) {
                        let idx: usize = o as usize;
                        let end = usize::min(
                            SPROT_TRANSFER_SIZE,
                            (measurement_len - o) as usize,
                        );
                        if idx + end >= buf.len() {
                            return Err(
                                AttestDataSprotError::CommsBufTooSmall.into()
                            );
                        }
                        self.sprot
                            .log(o, &mut buf[idx..idx + end])
                            .map_err(AttestDataSprotError::from)?;
                    }
                    Ok(measurement_len as usize)
                };
                self.tx_buf.try_encode_response(
                    sequence,
                    &SpToHost::RotResponse,
                    |buf| {
                        attest_data::messages::try_serialize(
                            buf,
                            &RotToHost::RotMeasurementLog,
                            f,
                        )
                        .map_err(|e| SpToHost::DecodeFailure(e.into()))
                    },
                );
            }
            HostToRotCommand::Attest => {
                let f = |buf: &mut [u8]| {
                    let attest_len: usize = self
                        .sprot
                        .attest_len()
                        .map_err(AttestDataSprotError::from)?
                        as usize;
                    if attest_len >= buf.len() {
                        return Err(
                            AttestDataSprotError::CommsBufTooSmall.into()
                        );
                    }
                    self.sprot
                        .attest(data, &mut buf[..attest_len])
                        .map_err(AttestDataSprotError::from)?;
                    Ok(attest_len)
                };
                self.tx_buf.try_encode_response(
                    sequence,
                    &SpToHost::RotResponse,
                    |buf| {
                        attest_data::messages::try_serialize(
                            buf,
                            &RotToHost::RotAttestation,
                            f,
                        )
                        .map_err(|e| SpToHost::DecodeFailure(e.into()))
                    },
                );
            }
            HostToRotCommand::GetTqCertificates => {
                let f = |buf: &mut [u8]| {
                    let mut cnt = 0;
                    let cert_chain_len = self
                        .sprot
                        .tq_cert_chain_len()
                        .map_err(AttestDataSprotError::from)?;
                    let mut buf_idx = 0;
                    for index in 0..cert_chain_len {
                        let cert_len = self
                            .sprot
                            .tq_cert_len(index)
                            .map_err(AttestDataSprotError::from)?;
                        for o in (0..cert_len).step_by(512) {
                            let idx: usize = buf_idx + o as usize;
                            let len = usize::min(512, (cert_len - o) as usize);

                            self.sprot
                                .tq_cert(index, o, &mut buf[idx..idx + len])
                                .map_err(AttestDataSprotError::from)?;
                        }
                        cnt += cert_len;
                        buf_idx += cert_len as usize;
                    }
                    Ok(cnt as usize)
                };
                self.tx_buf.try_encode_response(
                    sequence,
                    &SpToHost::RotResponse,
                    |buf| {
                        attest_data::messages::try_serialize(
                            buf,
                            &RotToHost::RotTqCertificates,
                            f,
                        )
                        .map_err(|e| SpToHost::DecodeFailure(e.into()))
                    },
                );
            }
            HostToRotCommand::TqSign => {
                let f = |buf: &mut [u8]| {
                    let sign_len: usize = self
                        .sprot
                        .tq_sign_len()
                        .map_err(AttestDataSprotError::from)?
                        as usize;
                    self.sprot
                        .tq_sign(data, &mut buf[..sign_len])
                        .map_err(AttestDataSprotError::from)?;
                    Ok(sign_len)
                };
                self.tx_buf.try_encode_response(
                    sequence,
                    &SpToHost::RotResponse,
                    |buf| {
                        attest_data::messages::try_serialize(
                            buf,
                            &RotToHost::RotTqSign,
                            f,
                        )
                        .map_err(|e| SpToHost::DecodeFailure(e.into()))
                    },
                );
            }
        };
    }

    /// On success, we will have already filled `self.tx_buf` with our response.
    /// On failure, our caller should response with
    /// `SpToHost::KeyLookupResult(err)` with the error we return.
    fn perform_key_lookup(
        &mut self,
        sequence: u64,
        key: u8,
        max_response_len: usize,
    ) -> Result<(), KeyLookupResult> {
        let key = Key::from_u8(key).ok_or(KeyLookupResult::InvalidKey)?;

        let response_len = match key {
            Key::Ping => {
                const PONG: &[u8] = b"pong";

                self.tx_buf.encode_response(
                    sequence,
                    &SpToHost::KeyLookupResult(KeyLookupResult::Ok),
                    |buf| {
                        buf[..PONG.len()].copy_from_slice(PONG);
                        PONG.len()
                    },
                );
                PONG.len()
            }
            Key::InstallinatorImageId => {
                // Borrow `cp_agent` to avoid borrowing `self` in the closure
                // below.
                let cp_agent = &self.cp_agent;

                // We don't want to have to set aside our own memory to copy the
                // installinator image ID (other than our already-allocated
                // outgoing tx buf), so we will optimistically serialize a
                // successful response, including the image ID. After
                // serializing this successful response, we'll check that
                // `max_response_len` (i.e., the buffer length of the host
                // process that requested this value) is sufficient; if it is
                // not (or if we have no installinator image ID at all), we'll
                // discard the optimistically-serialized response and return an
                // error.
                //
                // We expect both of these "reset and replace the response with
                // an error" to be extremely rare: host processes should not ask
                // for an installinator ID with a too-small buffer, and should
                // only ask for an installinator ID during a recovery process in
                // which we expect MGS has already given us an ID.
                let mut response_len = 0;
                self.tx_buf.encode_response(
                    sequence,
                    &SpToHost::KeyLookupResult(KeyLookupResult::Ok),
                    |mut buf| {
                        // Statically guarantee we have sufficient space in
                        // `buf` for the installinator image ID blob, and then
                        // cap `buf` to that length to satisfy the idol
                        // operation length limit.
                        const_assert!(
                            MIN_SP_TO_HOST_FILL_DATA_LEN
                                >= MAX_INSTALLINATOR_IMAGE_ID_LEN
                        );
                        buf = &mut buf[..MAX_INSTALLINATOR_IMAGE_ID_LEN];

                        response_len = cp_agent.get_installinator_image_id(buf);
                        response_len
                    },
                );

                // A response length of 0 is how `control-plane-agent` indicates
                // we do not have an installinator image ID; instead of
                // returning a 0-length success to the host, convert it to the
                // "we have no value for this key" error.
                if response_len == 0 {
                    self.tx_buf.reset();
                    return Err(KeyLookupResult::NoValueForKey);
                }
                response_len
            }
            Key::InventorySize => {
                // We reply with a tuple of count, API version:
                const REPLY: (u32, u32) =
                    (ServerImpl::INVENTORY_COUNT, INVENTORY_API_VERSION);
                let mut response_len = 0;
                self.tx_buf.encode_response(
                    sequence,
                    &SpToHost::KeyLookupResult(KeyLookupResult::Ok),
                    |buf| {
                        const_assert!(
                            MIN_SP_TO_HOST_FILL_DATA_LEN
                                >= core::mem::size_of::<u32>() * 2
                        );
                        response_len =
                            hubpack::serialize(buf, &REPLY).unwrap_lite();
                        response_len
                    },
                );
                response_len
            }
            Key::EtcSystem => {
                let response_len = self.host_kv_storage.etc_system_len;
                if response_len == 0 {
                    return Err(KeyLookupResult::NoValueForKey);
                }

                self.tx_buf.encode_response(
                    sequence,
                    &SpToHost::KeyLookupResult(KeyLookupResult::Ok),
                    |buf| {
                        // Statically guarantee we have sufficient space in
                        // `buf` for longest possible ETC_SYSTEM blob.
                        const_assert!(
                            MIN_SP_TO_HOST_FILL_DATA_LEN >= MAX_ETC_SYSTEM_LEN
                        );
                        buf[..response_len].copy_from_slice(
                            &self.host_kv_storage.etc_system[..response_len],
                        );
                        response_len
                    },
                );
                response_len
            }
            Key::DtraceConf => {
                let response_len = self.host_kv_storage.dtrace_conf_len;
                if response_len == 0 {
                    return Err(KeyLookupResult::NoValueForKey);
                }

                self.tx_buf.encode_response(
                    sequence,
                    &SpToHost::KeyLookupResult(KeyLookupResult::Ok),
                    |buf| {
                        const_assert!({
                            // `MIN_SP_TO_HOST_FILL_DATA_LEN` is calculated
                            // assuming `SpToHost::MAX_SIZE`, but we know in
                            // this callback we're appending to
                            // `SpToHost::KeyLookupResult(KeyLookupResult::Ok)`,
                            // which is only 2 bytes. Recompute the exact max
                            // space we have for our response, then statically
                            // guarantee we have sufficient space in `buf` for
                            // longest possible DTRACE_CONF blob.
                            #[allow(dead_code)] // suppress warning in nightly
                            const SP_TO_HOST_FILL_DATA_LEN: usize =
                                MIN_SP_TO_HOST_FILL_DATA_LEN
                                    + SpToHost::MAX_SIZE
                                    - 2;
                            SP_TO_HOST_FILL_DATA_LEN >= MAX_DTRACE_CONF_LEN
                        });

                        buf[..response_len].copy_from_slice(
                            &self.host_kv_storage.dtrace_conf[..response_len],
                        );
                        response_len
                    },
                );
                response_len
            }
        };

        if response_len > max_response_len {
            self.tx_buf.reset();
            Err(KeyLookupResult::MaxResponseLenTooShort)
        } else {
            Ok(())
        }
    }
}

impl NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::USART_IRQ_MASK
            | notifications::JEFE_STATE_CHANGE_MASK
            | notifications::MULTITIMER_MASK
            | notifications::CONTROL_PLANE_AGENT_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        if bits & notifications::USART_IRQ_MASK != 0 {
            self.handle_usart_notification();
            sys_irq_control(notifications::USART_IRQ_MASK, true);
        }

        if bits & notifications::JEFE_STATE_CHANGE_MASK != 0 {
            self.handle_jefe_notification(self.sequencer.get_state());
        }

        if bits & notifications::CONTROL_PLANE_AGENT_MASK != 0 {
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
                        self.rx_buf,
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

// This is conceptually a method on `ServerImpl`, but it takes a reference to
// `rx_buf` instead of `self` to avoid borrow checker issues.
fn parse_received_message(
    rx_buf: &mut [u8],
) -> Result<(Header, HostToSp, &[u8]), DecodeFailureReason> {
    let n = corncobs::decode_in_place(rx_buf)
        .map_err(|_| DecodeFailureReason::Cobs)?;
    let deframed = &rx_buf[..n];

    let (header, request, data) =
        host_sp_messages::deserialize::<HostToSp>(deframed)?;

    if header.magic != host_sp_messages::MAGIC {
        return Err(DecodeFailureReason::MagicMismatch);
    }

    if header.version != host_sp_messages::version::V1 {
        return Err(DecodeFailureReason::VersionMismatch);
    }

    if header.sequence & SEQ_REPLY != 0 {
        return Err(DecodeFailureReason::SequenceInvalid);
    }

    Ok((header, request, data))
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
        if uart.try_tx_push(0) {
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
}

// Borrow checker workaround; list of actions we perform in response to a host
// request _after_ we're done borrowing any message buffers.
enum Action {
    RebootHost,
    PowerOffHost,
    ClearStatusBits(Status),
    HfMuxToSP,
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
        } else if #[cfg(feature = "usart6")] {
            const PINS: &[(PinSet, Alternate)] = {
                cfg_if::cfg_if! {
                    if #[cfg(feature = "hardware_flow_control")] {
                        &[(
                            Port::G.pin(8).and_pin(9).and_pin(14).and_pin(15),
                            Alternate::AF7
                        )]
                    } else {
                        compile_error!("hardware_flow_control should be enabled");
                    }
                }
            };
            let usart = unsafe { &*device::USART6::ptr() };
            let peripheral = Peripheral::Usart6;
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
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
    ))] {
        // This net is named SP_TO_SP3_INT_L in the schematic
        const SP_TO_HOST_CPU_INT_L: sys_api::PinSet = sys_api::Port::I.pin(7);
    } else if #[cfg(target_board = "gimletlet-2")] {
        // gimletlet doesn't have an SP3 to interrupt, but we can wire up an LED
        // to one of the exposed E2-E6 pins to see it visually.
        const SP_TO_HOST_CPU_INT_L: sys_api::PinSet = sys_api::Port::E.pin(2);
    } else if #[cfg(target_board = "grapefruit")] {
        // the CPU interrupt is not connected on grapefruit, so pick an
        // unconnected GPIO
        const SP_TO_HOST_CPU_INT_L: sys_api::PinSet = sys_api::Port::B.pin(1);
    } else {
        compile_error!("unsupported target board");
    }
}

fn sp_to_sp3_interrupt_enable(sys: &sys_api::Sys) {
    sys.gpio_set(SP_TO_HOST_CPU_INT_L);

    sys.gpio_configure_output(
        SP_TO_HOST_CPU_INT_L,
        sys_api::OutputType::OpenDrain,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );
}

mod idl {
    use task_host_sp_comms_api::{HostSpCommsError, Status};
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
