// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use gateway_messages::{Header, Message, MessageKind, SpPort, SpRequest};
use heapless::Vec;
use idol_runtime::{Leased, RequestError};
use task_control_plane_agent_api::ControlPlaneAgentError;
use task_net_api::{Address, Ipv6Address, UdpMetadata};
use userlib::{sys_get_timer, sys_post, TaskId, UnwrapLite};

const SP_TO_MGS_MULTICAST_ADDR: Address = Address::Ipv6(Ipv6Address([
    0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0xde, 0, 1,
]));
const MGS_UDP_PORT: u16 = 22222;

// We only support a single in-flight host phase 2 request at a time (i.e., we
// do not support any pipelining). When the host makes a request of us (via
// `host-sp-comms`, which calls our idol interface on its behalf), we may not
// know _which_ MGS instance is able to provide the data it wants, and given
// we're using UDP, we need to support a small number of retries. Our constants
// here control how long we wait before we try "the other" MGS (assuming we've
// picked one to start with), and then how long we wait before switching back
// and retrying both. Assuming no successful replies from either MGS instance,
// our timeline for a request will be:
//
// 1. Start in `State::NeedToSendFirstMgs(port)`.
// 2. We send a request on our currently-selected port and update our state to
//    `State::WaitingForFirstMgs { .. }`.
// 2. If we have not received a successful response after `DELAY_TRY_OTHER_MGS`
//    ticks, we will switch to the other port, send a request, and update our
//    state to `State::WaitingForSecondMgs { .. }`.
// 3. If we have not received a successful response after `DELAY_RETRY` _and_ we
//    have attempted fewer than `MAX_ATTEMPTS`, we will switch back to the first
//    port and go to step 2. If we have exceeded `MAX_ATTEMPTS`, we'll notify
//    our caller (and give them an error when they request the data).
//
// The overall flow of data is:
//
// 1. Host requests data over the uart to `host-sp-comms`
// 2. `host-sp-comms` calls our `fetch_host_phase2_data` idol function, which
//    results in a call to `start_fetch()` below. We record the requesting task
//    ID and a notification bit but return immediately.
// 3. We request data from MGS; this follows the process above to attempt both
//    ports, retry, etc.
// 4. Once data arrives from MGS (or if we give up after `MAX_ATTEMPTS`), we
//    notify `host-sp-comms` that the fetch is complete.
// 5. `host-sp-comms` calls our `get_host_phase2_data` idol function, which
//    results in a call to `get_data()` below.
// 6. `host-sp-comms` relays the data (or failure) back to the host over the
//    uart.
const DELAY_TRY_OTHER_MGS: u64 = 500;
const DELAY_RETRY: u64 = 1_000;
const MAX_ATTEMPTS: u8 = 6;

pub(super) type Phase2Buf = Vec<u8, { gateway_messages::MAX_SERIALIZED_SIZE }>;

pub(crate) struct HostPhase2Requester {
    current: Option<CurrentRequest>,
    last_responsive_mgs: SpPort,
    buffer: &'static mut Phase2Buf,
}

impl HostPhase2Requester {
    // This function can only be called once;
    pub(super) fn new(buffer: &'static mut Phase2Buf) -> Self {
        Self {
            current: None,
            last_responsive_mgs: SpPort::One,
            buffer,
        }
    }

    pub(crate) fn start_fetch(
        &mut self,
        requesting_task: TaskId,
        requesting_task_notification_bit: u8,
        hash: [u8; 32],
        offset: u64,
    ) {
        self.current = Some(CurrentRequest {
            requesting_task,
            requesting_task_notification_bit,
            hash,
            offset,
            state: State::NeedToSendFirstMgs(self.last_responsive_mgs),
            retry_count: 0,
        });
        self.buffer.clear();
    }

    pub(crate) fn timer_deadline(&self) -> Option<u64> {
        self.current
            .as_ref()
            .and_then(|current| current.state.timer_deadline())
    }

    pub(crate) fn wants_to_send_packet(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|c| c.state.wants_to_send_packet())
    }

    pub(crate) fn packet_to_mgs(
        &mut self,
        message_id: u32,
        tx_buf: &mut [u8; gateway_messages::MAX_SERIALIZED_SIZE],
    ) -> Option<UdpMetadata> {
        let current = self.current.as_mut()?;

        let now = sys_get_timer().now;

        // Are we in a state where we should send a packet? If so, extract which
        // port we should use, and update `current.state` assuming that packet
        // will be sent imminently (which will be true as long as there's room
        // in our outgoing net task queue).
        let port = match current.state {
            State::NeedToSendFirstMgs(port) => {
                // Using saturating_add here because it's cheaper than
                // panicking, and timestamps won't saturate for 584 million
                // years.
                current.state = State::WaitingForFirstMgs {
                    port,
                    deadline: now.saturating_add(DELAY_TRY_OTHER_MGS),
                };
                port
            }
            State::WaitingForFirstMgs { port, deadline } => {
                if now < deadline {
                    return None;
                }
                // Timed out waiting for a response from the first MGS we tried;
                // flip to the other one.
                let port = match port {
                    SpPort::One => SpPort::Two,
                    SpPort::Two => SpPort::One,
                };
                current.state = State::WaitingForSecondMgs {
                    port,
                    deadline: now.saturating_add(DELAY_RETRY),
                };
                port
            }
            State::WaitingForSecondMgs { port, deadline } => {
                if now < deadline {
                    return None;
                }
                // Timed out waiting for a response from the second MGS we
                // tried; flip back to the first and retry.
                current.retry_count += 1;
                if current.retry_count >= MAX_ATTEMPTS {
                    current.notify_calling_task();
                    self.current = None;
                    return None;
                }
                let port = match port {
                    SpPort::One => SpPort::Two,
                    SpPort::Two => SpPort::One,
                };
                current.state = State::WaitingForFirstMgs {
                    port,
                    deadline: now.saturating_add(DELAY_TRY_OTHER_MGS),
                };
                port
            }
            State::Fetched => return None,
        };

        let message = Message {
            header: Header {
                version: gateway_messages::version::CURRENT,
                message_id,
            },
            kind: MessageKind::SpRequest(SpRequest::HostPhase2Data {
                hash: current.hash,
                offset: current.offset,
            }),
        };

        let n = gateway_messages::serialize(tx_buf, &message).unwrap_lite();

        let vid = match port {
            SpPort::One => task_net_api::VLanId::Sidecar1,
            SpPort::Two => task_net_api::VLanId::Sidecar2,
        };

        Some(UdpMetadata {
            addr: SP_TO_MGS_MULTICAST_ADDR,
            port: MGS_UDP_PORT,
            size: n as u32,
            vid,
        })
    }

    pub(crate) fn ingest_incoming_data(
        &mut self,
        port: SpPort,
        hash: [u8; 32],
        offset: u64,
        data: &[u8],
    ) {
        let current = match self.current.as_mut() {
            Some(current)
                if hash == current.hash && offset == current.offset =>
            {
                current
            }
            // If we either don't have a `current` request or the data we've
            // just received doesn't match our current request, we can silently
            // discard it. This shouldn't be common but is entirely possible if
            // we've sent multiple requests for the same data (particularly to
            // both MGS instances). One possible such sequence:
            //
            // 1. Host requests data at offset 0
            // 2. We send a request for offset 0 to MGS 0
            // 3. No response within our timeout window, so we send a request
            //    for offset 0 to MGS 1
            // 4. MGS 1 responds with data at offset 0; we forward it back to
            //    the host
            // 5. Host requests data at offset N
            // 6. We send a request for offset N to MGS 1
            // 7. MGS 0 finally replies to the request for data at offset 0 we
            //    sent back in step 1; we no longer care about this data, so we
            //    ignore it here.
            _ => return,
        };

        // Have we already ingested the data we need for our request? If so, we
        // again have sent multiple requests, and have received multiple
        // (presumed duplicate!) replies.
        if matches!(current.state, State::Fetched) {
            return;
        }

        // If we're expecting to ingest data, our buffer should be empty.
        assert!(self.buffer.is_empty());

        // Given `self.buffer` is sized to
        // `gateway_messages::MAX_SERIALIZED_SIZE` and `data` is coming from MGS
        // (so therefore <= `gateway_messages::MAX_SERIALIZED_SIZE`), we always
        // expect this `min` to return `data.len()`. If somehow we end up in a
        // position where that isn't the case, this will still work but will
        // discard some data, wasting some bandwidth but otherwise not having an
        // adverse effect.
        let n = usize::min(self.buffer.capacity(), data.len());
        self.buffer.extend_from_slice(&data[..n]).unwrap_lite();

        current.state = State::Fetched;
        current.notify_calling_task();
        self.last_responsive_mgs = port;
    }

    pub(crate) fn get_data(
        &self,
        hash: [u8; 32],
        offset: u64,
        data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        match self.current.as_ref() {
            Some(current)
                if hash == current.hash && offset == current.offset =>
            {
                let n = usize::min(data.len(), self.buffer.len());
                data.write_range(0..n, &self.buffer[..n])
                    .map_err(|()| RequestError::went_away())?;
                Ok(n)
            }
            _ => Err(ControlPlaneAgentError::DataUnavailable.into()),
        }
    }
}

struct CurrentRequest {
    requesting_task: TaskId,
    requesting_task_notification_bit: u8,
    hash: [u8; 32],
    offset: u64,
    state: State,
    retry_count: u8,
}

impl CurrentRequest {
    fn notify_calling_task(&self) {
        sys_post(
            self.requesting_task,
            1 << self.requesting_task_notification_bit,
        );
    }
}

enum State {
    NeedToSendFirstMgs(SpPort),
    WaitingForFirstMgs { port: SpPort, deadline: u64 },
    WaitingForSecondMgs { port: SpPort, deadline: u64 },
    Fetched,
}

impl State {
    /// Returns `true` if we want to send a packet
    fn wants_to_send_packet(&self) -> bool {
        match self {
            State::NeedToSendFirstMgs(..) => true,
            State::WaitingForFirstMgs { deadline, .. }
            | State::WaitingForSecondMgs { deadline, .. } => {
                // If we've timed out, then we want to send a new packet to the
                // opposite port.
                sys_get_timer().now >= *deadline
            }
            State::Fetched => false,
        }
    }

    fn timer_deadline(&self) -> Option<u64> {
        match self {
            State::NeedToSendFirstMgs(_) => Some(sys_get_timer().now),
            State::Fetched => None,
            State::WaitingForFirstMgs { deadline, .. }
            | State::WaitingForSecondMgs { deadline, .. } => Some(*deadline),
        }
    }
}
