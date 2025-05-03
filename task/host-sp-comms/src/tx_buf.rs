// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Trace, MAX_MESSAGE_SIZE, MAX_PACKET_SIZE};
use core::ops::Range;
use host_sp_messages::{
    DecodeFailureReason, Header, InventoryData, InventoryDataResult, SpToHost,
};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use userlib::{sys_get_timer, UnwrapLite};

/// We set the high bit of the sequence number before replying to host requests.
const SEQ_REPLY: u64 = 0x8000_0000_0000_0000;

#[derive(Debug, Clone, Copy)]
pub(super) struct WaitingForPhase2Data {
    pub(super) sequence: u64,
    pub(super) hash: [u8; 32],
    pub(super) offset: u64,
}

pub(super) struct TxBuf {
    // Staging area for an unencoded message.
    msg: &'static mut [u8; MAX_MESSAGE_SIZE],
    // Buffer for a corncobs-encoded packet, including the nul terminator.
    //
    // We bump this size up by one to allow us to prefix messages with a 0x00
    // byte also, which is normally unnecessary but required in the event we
    // need to reset possibly mid-packet. We always leave a 0 byte at the front
    // of `pkt`.
    pkt: &'static mut [u8; MAX_PACKET_SIZE + 1],
    state: State,
}

pub(super) struct StaticBufs {
    msg: [u8; MAX_MESSAGE_SIZE],
    pkt: [u8; MAX_PACKET_SIZE + 1],
}

impl StaticBufs {
    pub(super) const fn new() -> Self {
        Self {
            msg: [0; MAX_MESSAGE_SIZE],
            pkt: [0; MAX_PACKET_SIZE + 1],
        }
    }
}

impl TxBuf {
    pub(crate) fn new(
        StaticBufs {
            ref mut msg,
            ref mut pkt,
        }: &'static mut StaticBufs,
    ) -> Self {
        Self {
            msg,
            pkt,
            state: State::Idle,
        }
    }

    /// Reset to an idle state; if we had data we were sending, it's possible
    /// we've sent a partial packet. We always prefix packets with a 0x00
    /// terminator, but such a case means the host will receive an incomplete
    /// packet.
    pub(crate) fn reset(&mut self) {
        ringbuf_entry!(Trace::ResponseBufferReset {
            now: sys_get_timer().now
        });
        self.state = State::Idle;
    }

    /// Set our state to note that we do not have a response to send currently
    /// because we are waiting for host phase2 data to arrive from
    /// `control-plane-agent`.
    ///
    /// While in this state:
    ///
    /// * `get_waiting_for_phase2_data()` returns `Some(_)`
    /// * `next_byte_to_send()` returns `None` (i.e., we have no data to send to
    ///   the host yet)
    /// * `should_send_periodic_zero_bytes()` returns false (i.e., we are _not_
    ///   in between request/response phases - the host is waiting for us to
    ///   send a response)
    ///
    /// # Panics
    ///
    /// If we are in any state other than `Idle`.
    pub(crate) fn set_waiting_for_phase2_data(
        &mut self,
        sequence: u64,
        hash: [u8; 32],
        offset: u64,
    ) {
        assert!(matches!(self.state, State::Idle));
        self.state = State::WaitingForPhase2Data(WaitingForPhase2Data {
            sequence,
            hash,
            offset,
        });
    }

    /// If we're waiting for phase 2 data, returns the hash and offset of the
    /// desired data.
    pub(crate) fn get_waiting_for_phase2_data(
        &self,
    ) -> Option<WaitingForPhase2Data> {
        match &self.state {
            State::Idle | State::ToSend(_) => None,
            State::WaitingForPhase2Data(w) => Some(*w),
        }
    }

    /// Returns the next byte we should send, if we have one.
    pub(crate) fn next_byte_to_send(&self) -> Option<u8> {
        if let State::ToSend(r) = &self.state {
            Some(self.pkt[r.start])
        } else {
            None
        }
    }

    /// Advance past the first byte of the data we have to send.
    ///
    /// Should only be called after `next_byte_to_send()` has returned
    /// `Some(b)`, and `b` has successfully been queued into the TX FIFO.
    pub(crate) fn advance_one_byte(&mut self) {
        match &mut self.state {
            State::Idle | State::WaitingForPhase2Data { .. } => panic!(),
            State::ToSend(r) => {
                r.start += 1;
                if r.start == r.end {
                    self.state = State::Idle;
                }
            }
        }
    }

    /// Should we be sending periodic 0 bytes?
    #[cfg(feature = "gimlet")]
    pub(crate) fn should_send_periodic_zero_bytes(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    #[cfg(any(feature = "grapefruit", feature = "cosmo"))]
    pub(crate) fn should_send_periodic_zero_bytes(&self) -> bool {
        false
    }

    /// Encodes `reason` into our outgoing buffer.
    ///
    /// # Panics
    ///
    /// If we still have data from a previously-encoded message that hasn't been
    /// sent.
    pub(crate) fn encode_decode_failure_reason(
        &mut self,
        reason: DecodeFailureReason,
    ) {
        assert!(!matches!(self.state, State::ToSend(_)));
        let header = Header {
            magic: host_sp_messages::MAGIC,
            version: host_sp_messages::version::V1,
            // We failed to decode, so don't know the sequence number.
            sequence: 0xffff_ffff_ffff_ffff,
        };
        let response = SpToHost::DecodeFailure(reason);

        // Serializing can only fail if we pass unexpected types as `response`,
        // but we're using `SpToHost`, so it cannot fail.
        let n = host_sp_messages::try_serialize(
            self.msg,
            &header,
            &response,
            |_| Ok(0),
        )
        .unwrap_lite();

        // Corncobs-encode the serialized response.
        self.encode_message(n);
    }

    /// Encodes `response` into our outgoing buffer, setting the `SEQ_REPLY` bit
    /// in the header sequence number.
    ///
    /// # Panics
    ///
    /// If we still have data from a previously-encoded message that hasn't been
    /// sent.
    pub(crate) fn encode_response<F>(
        &mut self,
        sequence: u64,
        response: &SpToHost,
        fill_data: F,
    ) where
        F: FnOnce(&mut [u8]) -> usize,
    {
        self.try_encode_response(sequence, response, |buf| Ok(fill_data(buf)))
    }

    pub(crate) fn try_encode_inventory<'a, F>(
        &mut self,
        sequence: u64,
        name: &'a [u8],
        fill_data: F,
    ) where
        F: FnOnce() -> Result<&'a InventoryData, InventoryDataResult>,
    {
        let mut name_array = [0u8; 32];
        let n = name_array.len().min(name.len());
        name_array[..n].copy_from_slice(&name[..n]);
        self.try_encode_response(
            sequence,
            &SpToHost::InventoryData {
                result: InventoryDataResult::Ok,
                name: name_array,
            },
            |buf| {
                let n = fill_data()
                    .and_then(|d| {
                        hubpack::serialize(buf, d).map_err(|_| {
                            InventoryDataResult::SerializationError
                        })
                    })
                    .map_err(|e| SpToHost::InventoryData {
                        result: e,
                        name: name_array,
                    })?;

                Ok(n)
            },
        )
    }

    /// Encodes `response` into our outgoing buffer, setting the `SEQ_REPLY` bit
    /// in the header sequence number.  If the `fill_data` callback returns an
    /// error, serialize that error instead of the given `response` (which is of
    /// the same type).
    ///
    /// # Panics
    ///
    /// If we still have data from a previously-encoded message that hasn't been
    /// sent.
    pub(crate) fn try_encode_response<F>(
        &mut self,
        sequence: u64,
        response: &SpToHost,
        fill_data: F,
    ) where
        F: FnOnce(&mut [u8]) -> Result<usize, SpToHost>,
    {
        assert!(!matches!(self.state, State::ToSend(_)));

        let n = self.try_serialize_response(sequence, response, fill_data);
        self.encode_message(n);
    }

    fn try_serialize_response<F>(
        &mut self,
        sequence: u64,
        response: &SpToHost,
        fill_data: F,
    ) -> usize
    where
        F: FnOnce(&mut [u8]) -> Result<usize, SpToHost>,
    {
        let header = Header {
            magic: host_sp_messages::MAGIC,
            version: host_sp_messages::version::V1,
            sequence: sequence | SEQ_REPLY,
        };

        ringbuf_entry!(Trace::Response {
            now: sys_get_timer().now,
            sequence: header.sequence,
            message: *response
        });

        // Serializing can only fail if we pass unexpected types as `response`,
        // but we're using `SpToHost` for both the response and error, so it
        // cannot fail.
        host_sp_messages::try_serialize(self.msg, &header, response, fill_data)
            .unwrap_lite()
    }

    // Encodes `self.msg[..msg_len]` with corncobs.
    fn encode_message(&mut self, msg_len: usize) {
        // We write to `self.pkt[1..]` but note that we need to send `0..n+1` so
        // that all packets are prefixed with a terminator, in case the previous
        // packet was only partially sent (if we were `reset()`).
        let n = corncobs::encode_buf(&self.msg[..msg_len], &mut self.pkt[1..]);
        self.state = State::ToSend(0..n + 1);
    }
}

enum State {
    // We've responded to the most recent host request (if any), and are waiting
    // for a new request to come in.
    Idle,
    // We're waiting for host phase 2 data from control-plane-agent.
    WaitingForPhase2Data(WaitingForPhase2Data),
    // We have data to send; the associated range describes the bytes in
    // `TxBuf::pkt` we still need to transmit.
    ToSend(Range<usize>),
}
