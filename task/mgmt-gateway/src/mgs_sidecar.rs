// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::convert::Infallible;

use crate::{mgs_common::MgsCommon, Log, MgsMessage};
use drv_sidecar_seq_api::Sequencer;
use gateway_messages::{
    sp_impl::SocketAddrV6, sp_impl::SpHandler, BulkIgnitionState,
    DiscoverResponse, IgnitionCommand, IgnitionState, PowerState,
    ResponseError, SpComponent, SpPort, SpState, UpdateChunk, UpdateId,
    UpdatePrepare, UpdateStatus,
};
use ringbuf::ringbuf_entry_root;
use task_net_api::UdpMetadata;

userlib::task_slot!(SIDECAR_SEQ, sequencer);

pub(crate) struct MgsHandler {
    common: MgsCommon,
    sequencer: Sequencer,
}

impl MgsHandler {
    /// Instantiate an `MgsHandler` that claims static buffers and device
    /// resources. Can only be called once; will panic if called multiple times!
    pub(crate) fn claim_static_resources() -> Self {
        Self {
            common: MgsCommon::claim_static_resources(),
            sequencer: Sequencer::from(SIDECAR_SEQ.get_task_id()),
        }
    }

    /// If we want to be woken by the system timer, we return a deadline here.
    /// `main()` is responsible for calling this method and actually setting the
    /// timer.
    pub(crate) fn timer_deadline(&self) -> Option<u64> {
        None
    }

    pub(crate) fn handle_timer_fired(&mut self) {}

    pub(crate) fn drive_usart(&mut self) {}

    pub(crate) fn wants_to_send_packet_to_mgs(&mut self) -> bool {
        false
    }

    pub(crate) fn packet_to_mgs(
        &mut self,
        _tx_buf: &mut [u8; gateway_messages::MAX_SERIALIZED_SIZE],
    ) -> Option<UdpMetadata> {
        None
    }
}

impl SpHandler for MgsHandler {
    fn discover(
        &mut self,
        _sender: SocketAddrV6,
        port: SpPort,
    ) -> Result<DiscoverResponse, ResponseError> {
        self.common.discover(port)
    }

    fn ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
    ) -> Result<IgnitionState, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionState {
            target
        }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn bulk_ignition_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<BulkIgnitionState, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::BulkIgnitionState));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn ignition_command(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionCommand {
            target,
            command
        }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn sp_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<SpState, ResponseError> {
        self.common.sp_state()
    }

    fn update_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        update: UpdatePrepare,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.total_size,
            component: update.component,
            id: update.id,
            slot: update.slot,
        }));

        match update.component {
            SpComponent::SP_ITSELF => self.common.update_prepare(update),
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
    }

    fn update_status(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
    ) -> Result<UpdateStatus, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateStatus {
            component
        }));

        match component {
            SpComponent::SP_ITSELF => Ok(self.common.status()),
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
    }

    fn update_chunk(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateChunk {
            component: chunk.component,
            offset: chunk.offset,
        }));

        match chunk.component {
            SpComponent::SP_ITSELF => self.common.update_chunk(chunk, data),
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
    }

    fn update_abort(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        component: SpComponent,
        id: UpdateId,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateAbort {
            component
        }));

        match component {
            SpComponent::SP_ITSELF => self.common.update_abort(&id),
            _ => Err(ResponseError::RequestUnsupportedForComponent),
        }
    }

    fn power_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<PowerState, ResponseError> {
        use drv_sidecar_seq_api::TofinoSeqState;
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetPowerState));

        // TODO Is this mapping of the sub-states correct? Do we want to expose
        // them to the control plane somehow (probably not)?
        let state = match self
            .sequencer
            .tofino_seq_state()
            .map_err(|e| ResponseError::PowerStateError(e as u32))?
        {
            TofinoSeqState::Initial
            | TofinoSeqState::InPowerDown
            | TofinoSeqState::A2 => PowerState::A2,
            TofinoSeqState::InPowerUp | TofinoSeqState::A0 => PowerState::A0,
        };

        Ok(state)
    }

    fn set_power_state(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        power_state: PowerState,
    ) -> Result<(), ResponseError> {
        use drv_sidecar_seq_api::TofinoSequencerPolicy;
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetPowerState(
            power_state
        )));

        let policy = match power_state {
            PowerState::A0 => TofinoSequencerPolicy::LatchOffOnFault,
            PowerState::A2 => TofinoSequencerPolicy::Disabled,
            PowerState::A1 => return Err(ResponseError::PowerStateError(0)),
        };

        self.sequencer
            .set_tofino_seq_policy(policy)
            .map_err(|e| ResponseError::PowerStateError(e as u32))
    }

    fn serial_console_attach(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        _component: SpComponent,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn serial_console_write(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleWrite {
            offset,
            length: data.len() as u16
        }));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn serial_console_detach(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), ResponseError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        Err(ResponseError::RequestUnsupportedForSp)
    }

    fn reset_prepare(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<(), ResponseError> {
        self.common.reset_prepare()
    }

    fn reset_trigger(
        &mut self,
        _sender: SocketAddrV6,
        _port: SpPort,
    ) -> Result<Infallible, ResponseError> {
        self.common.reset_trigger()
    }
}
