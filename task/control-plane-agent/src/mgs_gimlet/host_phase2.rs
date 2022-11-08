// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::vlan_id_from_sp_port;
use core::sync::atomic::{AtomicBool, Ordering};
use gateway_messages::{Header, Message, MessageKind, SpPort, SpRequest};
use heapless::Vec;
use idol_runtime::{Leased, RequestError};
use task_control_plane_agent_api::ControlPlaneAgentError;
use task_net_api::{Address, Ipv6Address, UdpMetadata};
use userlib::{sys_get_timer, sys_post, TaskId, UnwrapLite};

const ALL_NODES_MULTICAST: Address = Address::Ipv6(Ipv6Address([
    0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
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
const DELAY_TRY_OTHER_MGS: u64 = 500;
const DELAY_RETRY: u64 = 1_000;
const MAX_ATTEMPTS: u8 = 3;

pub(crate) struct HostPhase2Requester {
    current: Option<CurrentRequest>,
    last_responsive_mgs: SpPort,
    buffer: &'static mut Vec<u8, { gateway_messages::MAX_SERIALIZED_SIZE }>,
}

impl HostPhase2Requester {
    pub(crate) fn claim_static_resources() -> Self {
        Self {
            current: None,
            last_responsive_mgs: SpPort::One,
            buffer: claim_phase2_buffer(),
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
            .and_then(|c| c.state.port_to_send_packet())
            .is_some()
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
                current.state = State::WaitingForFirstMgs {
                    port,
                    deadline: now + DELAY_TRY_OTHER_MGS,
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
                    deadline: now + DELAY_RETRY,
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
                    deadline: now + DELAY_TRY_OTHER_MGS,
                };
                port
            }
            State::Fetched => return None,
        };

        let message = Message {
            header: Header {
                version: gateway_messages::version::V2,
                message_id,
            },
            kind: MessageKind::SpRequest(SpRequest::HostPhase2Data {
                hash: current.hash,
                offset: current.offset,
            }),
        };

        let n = gateway_messages::serialize(tx_buf, &message).unwrap_lite();

        Some(UdpMetadata {
            addr: ALL_NODES_MULTICAST,
            port: MGS_UDP_PORT,
            size: n as u32,
            vid: vlan_id_from_sp_port(port),
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
            _ => return,
        };

        self.buffer.clear();
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
    // If we want to send a packet, returns `Some(port)` on which we want to
    // send that packet.
    fn port_to_send_packet(&self) -> Option<SpPort> {
        match self {
            State::NeedToSendFirstMgs(port) => Some(*port),
            State::Fetched => None,
            State::WaitingForFirstMgs { deadline, port }
            | State::WaitingForSecondMgs { deadline, port } => {
                if sys_get_timer().now >= *deadline {
                    Some(*port)
                } else {
                    None
                }
            }
        }
    }

    fn timer_deadline(&self) -> Option<u64> {
        match self {
            State::NeedToSendFirstMgs(_) => Some(sys_get_timer().now + 1),
            State::Fetched => None,
            State::WaitingForFirstMgs { deadline, .. }
            | State::WaitingForSecondMgs { deadline, .. } => Some(*deadline),
        }
    }
}

fn claim_phase2_buffer(
) -> &'static mut Vec<u8, { gateway_messages::MAX_SERIALIZED_SIZE }> {
    static mut PHASE2_BUF: Vec<u8, { gateway_messages::MAX_SERIALIZED_SIZE }> =
        Vec::new();

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::Relaxed) {
        panic!()
    }

    // Safety: unsafe because of references to mutable statics; safe because of
    // the AtomicBool swap above, combined with the lexical scoping of
    // `PHASE2_BUF`, means that this reference can't be aliased by any
    // other reference in the program.
    unsafe { &mut PHASE2_BUF }
}
