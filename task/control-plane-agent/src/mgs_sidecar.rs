// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    mgs_common::MgsCommon, update::rot::RotUpdate, update::sp::SpUpdate,
    update::ComponentUpdater, usize_max, CriticalEvent, Log, MgsMessage,
};
use drv_ignition_api::IgnitionError;
use drv_monorail_api::{Monorail, MonorailError};
use drv_sidecar_seq_api::Sequencer;
use drv_transceivers_api::Transceivers;
use enum_map::EnumMap;
use gateway_messages::sp_impl::{
    BoundsChecked, DeviceDescription, Sender, SpHandler,
};
use gateway_messages::{
    ignition, ComponentAction, ComponentActionResponse, ComponentDetails,
    ComponentUpdatePrepare, DiscoverResponse, DumpSegment, DumpTask,
    EcdsaSha2Nistp256Challenge, IgnitionCommand, IgnitionState, MgsError,
    MgsRequest, MgsResponse, MonorailComponentAction,
    MonorailComponentActionResponse, MonorailError as GwMonorailError,
    PowerState, RotBootInfo, RotRequest, RotResponse, SensorRequest,
    SensorResponse, SpComponent, SpError, SpStateV2, SpUpdatePrepare,
    UnlockChallenge, UnlockResponse, UpdateChunk, UpdateId, UpdateStatus,
};
use host_sp_messages::HostStartupOptions;
use idol_runtime::{Leased, RequestError};
use ringbuf::{counted_ringbuf, ringbuf_entry, ringbuf_entry_root};
use task_control_plane_agent_api::{ControlPlaneAgentError, VpdIdentity};
use task_net_api::{MacAddress, UdpMetadata, VLanId};
use userlib::sys_get_timer;
use zerocopy::IntoBytes;

// We're included under a special `path` cfg from main.rs, which confuses rustc
// about where our submodules live. Pass explicit paths to correct it.
#[path = "mgs_sidecar/ignition.rs"]
mod ignition_handler;
#[path = "mgs_sidecar/monorail_port_status.rs"]
mod monorail_port_status;

use ignition_handler::IgnitionController;

userlib::task_slot!(SIDECAR_SEQ, sequencer);
userlib::task_slot!(MONORAIL, monorail);
userlib::task_slot!(TRANSCEIVERS, transceivers);
userlib::task_slot!(RNG, rng_driver);

#[allow(dead_code)] // Not all cases are used by all variants
#[derive(Clone, Copy, PartialEq, ringbuf::Count)]
enum Trace {
    #[count(skip)]
    None,
    TrustVLanFailed(task_net_api::TrustError),
    DistrustVLanFailed(task_net_api::TrustError),
    UnlockRequested {
        #[count(children)]
        vid: VLanId,
    },
    UnlockAuthFailed,
    UnlockAuthSucceeded,
    UnlockedUntil {
        #[count(children)]
        vid: VLanId,
        until: u64,
    },
    MonorailUnlockFailed(drv_monorail_api::MonorailError),
    TimedLockFailed(gateway_messages::MonorailError),
    TimedRelock {
        #[count(children)]
        vid: VLanId,
    },
    ExplicitRelock {
        #[count(children)]
        vid: VLanId,
    },
    Locking {
        #[count(children)]
        vid: VLanId,
    },
    MessageTrusted {
        #[count(children)]
        vid: VLanId,
    },
    MessageNotTrusted {
        #[count(children)]
        vid: VLanId,
    },
    NoChallenge,
    WrongChallenge,
    UnlockTimeTooLong {
        time_sec: u32,
    },
    ChallengeExpired,
    WrongKey,
    UntrustedResponse(GwMonorailError),
    RngFillFailed(drv_rng_api::RngError),
}
counted_ringbuf!(Trace, 16, Trace::None);

const CHALLENGE_EXPIRATION_TIME_SECS: u64 = 60;
const MAX_UNLOCK_TIME_SECS: u32 = 3600;

// How big does our shared update buffer need to be? Has to be able to handle SP
// update blocks for now, no other updateable components.
const UPDATE_BUFFER_SIZE: usize =
    usize_max(SpUpdate::BLOCK_SIZE, RotUpdate::BLOCK_SIZE);

// Create type aliases that include our `UpdateBuffer` size (i.e., the size of
// the largest update chunk of all the components we update).
pub(crate) type UpdateBuffer =
    update_buffer::UpdateBuffer<SpComponent, UPDATE_BUFFER_SIZE>;
pub(crate) type BorrowedUpdateBuffer = update_buffer::BorrowedUpdateBuffer<
    'static,
    SpComponent,
    UPDATE_BUFFER_SIZE,
>;

// Our single, shared update buffer.
static UPDATE_MEMORY: UpdateBuffer = UpdateBuffer::new();

#[derive(Copy, Clone, Debug)]
enum LockState {
    Locked,
    UnlockedUntil(u64),
    AlwaysUnlocked,
}

pub(crate) struct MgsHandler {
    common: MgsCommon,
    sequencer: Sequencer,
    monorail: Monorail,
    transceivers: Transceivers,
    ignition: IgnitionController,

    last_challenge: Option<(UnlockChallenge, u64)>,

    locked: EnumMap<VLanId, LockState>,
}

impl MgsHandler {
    /// Instantiate an `MgsHandler` that claims static buffers and device
    /// resources. Can only be called once; will panic if called multiple times!
    pub(crate) fn claim_static_resources(base_mac_address: MacAddress) -> Self {
        Self {
            common: MgsCommon::claim_static_resources(base_mac_address),
            sequencer: Sequencer::from(SIDECAR_SEQ.get_task_id()),
            monorail: Monorail::from(MONORAIL.get_task_id()),
            transceivers: Transceivers::from(TRANSCEIVERS.get_task_id()),
            ignition: IgnitionController::new(),

            last_challenge: None,
            locked: EnumMap::from_fn(|v: VLanId| {
                if v.cfg().always_trusted {
                    LockState::AlwaysUnlocked
                } else {
                    LockState::Locked
                }
            }),
        }
    }

    pub(crate) fn identity(&self) -> VpdIdentity {
        self.common.identity()
    }

    /// If we want to be woken by the system timer, we return a deadline here.
    /// `main()` is responsible for calling this method and actually setting the
    /// timer.
    pub(crate) fn timer_deadline(&self) -> Option<u64> {
        if self.common.sp_update.is_preparing() {
            Some(sys_get_timer().now + 1)
        } else {
            // Find the soonest LockState::UnlockedUntil time (if present)
            // TODO replace with a multitimer?
            self.locked
                .values()
                .flat_map(|k| match k {
                    LockState::UnlockedUntil(t) => Some(*t),
                    _ => None,
                })
                .min()
        }
    }

    pub(crate) fn handle_timer_fired(&mut self) {
        // This is a no-op if we're not preparing for an SP update.
        self.common.sp_update.step_preparation();

        let now = sys_get_timer().now;
        for (vid, k) in self.locked.clone().iter() {
            if let LockState::UnlockedUntil(lock_at) = k {
                if now >= *lock_at {
                    ringbuf_entry!(Trace::TimedRelock { vid });
                    if let Err(e) = self.lock(vid) {
                        ringbuf_entry!(Trace::TimedLockFailed(e));
                    }
                }
            }
        }
    }

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

    pub(crate) fn fetch_host_phase2_data(
        &mut self,
        _msg: &userlib::RecvMessage,
        _image_hash: [u8; 32],
        _offset: u64,
        _notification_bit: u8,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        Err(ControlPlaneAgentError::DataUnavailable.into())
    }

    pub(crate) fn get_host_phase2_data(
        &mut self,
        _image_hash: [u8; 32],
        _offset: u64,
        _data: Leased<idol_runtime::W, [u8]>,
    ) -> Result<usize, RequestError<ControlPlaneAgentError>> {
        Err(ControlPlaneAgentError::DataUnavailable.into())
    }

    pub(crate) fn startup_options_impl(
        &self,
    ) -> Result<HostStartupOptions, RequestError<ControlPlaneAgentError>> {
        // We don't have a host to give startup options; no one should be
        // calling this method.
        Err(ControlPlaneAgentError::InvalidStartupOptions.into())
    }

    pub(crate) fn set_startup_options_impl(
        &mut self,
        _startup_options: HostStartupOptions,
    ) -> Result<(), RequestError<ControlPlaneAgentError>> {
        // We don't have a host to give startup options; no one should be
        // calling this method.
        Err(ControlPlaneAgentError::InvalidStartupOptions.into())
    }

    fn power_state_impl(&self) -> Result<PowerState, SpError> {
        use drv_sidecar_seq_api::TofinoSeqState;

        // TODO Is this mapping of the sub-states correct? Do we want to expose
        // them to the control plane somehow (probably not)?
        let state = match self
            .sequencer
            .tofino_seq_state()
            .map_err(|e| SpError::PowerStateError(e as u32))?
        {
            TofinoSeqState::Init
            | TofinoSeqState::InPowerDown
            | TofinoSeqState::A2 => PowerState::A2,
            TofinoSeqState::InPowerUp | TofinoSeqState::A0 => PowerState::A0,
        };

        Ok(state)
    }

    /// Unlocks the tech port if the challenge and response are compatible
    fn unlock(
        &mut self,
        vid: VLanId,
        challenge: UnlockChallenge,
        response: UnlockResponse,
        time_sec: u32,
    ) -> Result<(), GwMonorailError> {
        ringbuf_entry!(Trace::UnlockRequested { vid });

        if time_sec > MAX_UNLOCK_TIME_SECS {
            ringbuf_entry!(Trace::UnlockTimeTooLong { time_sec });
            return Err(GwMonorailError::TimeIsTooLong);
        }

        if vid.cfg().always_trusted {
            return Err(GwMonorailError::AlreadyTrusted);
        }

        // Callers only get one attempt per challenge; if they fail to authorize
        // the unlock (or something else goes wrong while communicating to
        // hardware), they have to ask for a new challenge.
        let Some((last_challenge, challenge_time)) =
            core::mem::take(&mut self.last_challenge)
        else {
            ringbuf_entry!(Trace::NoChallenge);
            return Err(GwMonorailError::UnlockAuthFailed);
        };

        if challenge != last_challenge {
            ringbuf_entry!(Trace::WrongChallenge);
            return Err(GwMonorailError::UnlockAuthFailed);
        }

        let now = sys_get_timer().now;
        if now >= challenge_time + CHALLENGE_EXPIRATION_TIME_SECS * 1000 {
            ringbuf_entry!(Trace::ChallengeExpired);
            return Err(GwMonorailError::UnlockAuthFailed);
        }

        // Check that the response is valid for our current challenge.
        match (challenge, response) {
            (
                UnlockChallenge::Trivial { timestamp: ts1 },
                UnlockResponse::Trivial { timestamp: ts2 },
            ) if ts1 == ts2 => Ok(()),
            (
                UnlockChallenge::EcdsaSha2Nistp256(data),
                UnlockResponse::EcdsaSha2Nistp256 {
                    key,
                    signer_nonce,
                    signature,
                },
            ) => verify_signature(&data, &key, &signer_nonce, &signature),
            _ => Err(GwMonorailError::UnlockAuthFailed),
        }?;
        ringbuf_entry!(Trace::UnlockAuthSucceeded);

        // Pick how long we'll trust things for
        let now = sys_get_timer().now;
        let trust_until = now + u64::from(time_sec) * 1000;

        // Reconfigure the management network for arbitrary SP access
        if let Err(e) = self.monorail.unlock_vlans(trust_until) {
            ringbuf_entry!(Trace::MonorailUnlockFailed(e));
            return Err(GwMonorailError::UnlockFailed);
        }

        // Reconfigure the net task to temporarily trust this port's VLAN
        let net = task_net_api::Net::from(crate::NET.get_task_id());
        if let Err(e) = net.trust_vlan(vid, trust_until) {
            ringbuf_entry!(Trace::TrustVLanFailed(e));
            return Err(GwMonorailError::UnlockFailed);
        }
        ringbuf_entry!(Trace::UnlockedUntil {
            vid,
            until: trust_until
        });

        // Reconfigure our own internal state to accept messages
        self.locked[vid] = LockState::UnlockedUntil(trust_until);

        Ok(())
    }

    fn lock(&mut self, vid: VLanId) -> Result<(), GwMonorailError> {
        ringbuf_entry!(Trace::Locking { vid });
        self.locked[vid] = LockState::Locked;

        let net = task_net_api::Net::from(crate::NET.get_task_id());
        net.distrust_vlan(vid).map_err(|e| {
            ringbuf_entry!(Trace::DistrustVLanFailed(e));
            GwMonorailError::LockFailed
        })?;
        self.monorail.lock_vlans().map_err(|e| {
            ringbuf_entry!(Trace::MonorailUnlockFailed(e));
            GwMonorailError::LockFailed
        })?;

        Ok(())
    }

    fn ensure_sender_trusted<T>(
        &mut self,
        message: T,
        sender: Sender<VLanId>,
    ) -> Result<T, GwMonorailError> {
        let vid = sender.vid;

        // If this message is arriving on a trusted VLAN, then the lock state is
        // irrelevant.
        let cfg = vid.cfg();
        if cfg.always_trusted {
            ringbuf_entry!(Trace::MessageTrusted { vid });
            return Ok(message);
        }

        if let LockState::UnlockedUntil(t) = self.locked[vid] {
            let now = sys_get_timer().now;
            if now < t {
                ringbuf_entry!(Trace::MessageTrusted { vid });
                Ok(message)
            } else {
                ringbuf_entry!(Trace::TimedRelock { vid });
                self.lock(vid)?;
                ringbuf_entry!(Trace::MessageNotTrusted { vid });
                Err(GwMonorailError::ManagementNetworkLocked)
            }
        } else {
            ringbuf_entry!(Trace::MessageNotTrusted { vid });
            Err(GwMonorailError::ManagementNetworkLocked)
        }
    }
}

fn verify_signature(
    data: &EcdsaSha2Nistp256Challenge,
    key: &[u8; 65],
    signer_nonce: &[u8; 8],
    signature: &[u8; 64],
) -> Result<(), GwMonorailError> {
    if !TRUSTED_KEYS.iter().any(|t| t == key) {
        ringbuf_entry!(Trace::WrongKey);
        return Err(GwMonorailError::UnlockAuthFailed);
    }

    let sig = p256::ecdsa::Signature::from_bytes(signature.as_slice().into())
        .map_err(|_| GwMonorailError::UnlockAuthFailed)?;

    let v = p256::ecdsa::VerifyingKey::from_encoded_point(
        &p256::EncodedPoint::from_bytes(key)
            .map_err(|_| GwMonorailError::UnlockAuthFailed)?,
    )
    .map_err(|_| GwMonorailError::UnlockAuthFailed)?;

    // Build an SSH signature blob to be verified
    //
    // See https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.sshsig
    // for the signature blob format
    #[rustfmt::skip]
    let mut buf = [
        // MAGIC_PREAMBLE
        b'S', b'S', b'H', b'S', b'I', b'G',

        // namespace
        0, 0, 0, 15, // length
        b'm', b'o', b'n', b'o', b'r', b'a', b'i', b'l', b'-',
        b'u', b'n', b'l', b'o', b'c', b'k',

        // reserved
        0, 0, 0, 0,

        // hash type
        0, 0, 0, 6, // length
        b's', b'h', b'a', b'2', b'5', b'6',

        // hash of our actual data (to be filled in)
        0, 0, 0, 32, // length
        0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0,
    ];

    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(data.as_bytes());
    hasher.update(signer_nonce);
    let hash = hasher.finalize();
    buf[43..].copy_from_slice(&hash);

    use p256::ecdsa::signature::Verifier;
    v.verify(&buf, &sig)
        .map_err(|_| GwMonorailError::UnlockAuthFailed)
}

impl SpHandler for MgsHandler {
    type BulkIgnitionStateIter = ignition_handler::BulkIgnitionStateIter;
    type BulkIgnitionLinkEventsIter =
        ignition_handler::BulkIgnitionLinkEventsIter;
    type VLanId = VLanId;

    /// Checks whether we trust the given message
    fn ensure_request_trusted(
        &mut self,
        kind: MgsRequest,
        sender: Sender<VLanId>,
    ) -> Result<MgsRequest, SpError> {
        // Certain messages are always trusted:
        //  - Discovery (for obvious reasons)
        //  - Messages needed for unlocking
        //      - Requesting a new challenge
        //      - Sending an unlock command
        //  - Disabling the watchdog after reset
        //
        //  The latter means that we can update a Sidecar's SP firmware through
        //  the technician port without having to unlock it **again** after it
        //  boots into the new SP firmware (which would be awkward).
        if matches!(
            kind,
            MgsRequest::Discover
                | MgsRequest::ComponentAction {
                    component: SpComponent::MONORAIL,
                    action: ComponentAction::Monorail(
                        MonorailComponentAction::RequestChallenge
                            | MonorailComponentAction::Unlock { .. },
                    )
                }
                | MgsRequest::DisableComponentWatchdog { .. }
        ) {
            return Ok(kind);
        }

        self.ensure_sender_trusted(kind, sender)
            .map_err(SpError::Monorail)
    }

    fn ensure_response_trusted(
        &mut self,
        kind: MgsResponse,
        sender: Sender<VLanId>,
    ) -> Option<MgsResponse> {
        match self.ensure_sender_trusted(kind, sender) {
            Ok(k) => Some(k),
            Err(e) => {
                ringbuf_entry!(Trace::UntrustedResponse(e));
                None
            }
        }
    }

    fn discover(
        &mut self,
        sender: Sender<VLanId>,
    ) -> Result<DiscoverResponse, SpError> {
        self.common.discover(sender.vid)
    }

    fn num_ignition_ports(&mut self) -> Result<u32, SpError> {
        self.ignition
            .num_ports()
            .map_err(sp_error_from_ignition_error)
    }

    fn ignition_state(&mut self, target: u8) -> Result<IgnitionState, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionState {
            target
        }));
        self.ignition
            .target_state(target)
            .map_err(sp_error_from_ignition_error)
    }

    fn bulk_ignition_state(
        &mut self,
        offset: u32,
    ) -> Result<Self::BulkIgnitionStateIter, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::BulkIgnitionState {
            offset
        }));
        self.ignition
            .bulk_state(offset)
            .map_err(sp_error_from_ignition_error)
    }

    fn ignition_link_events(
        &mut self,
        target: u8,
    ) -> Result<ignition::LinkEvents, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionLinkEvents {
            target
        }));
        self.ignition
            .target_link_events(target)
            .map_err(sp_error_from_ignition_error)
    }

    fn bulk_ignition_link_events(
        &mut self,
        offset: u32,
    ) -> Result<Self::BulkIgnitionLinkEventsIter, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::BulkIgnitionLinkEvents { offset }
        ));
        self.ignition
            .bulk_link_events(offset)
            .map_err(sp_error_from_ignition_error)
    }

    fn clear_ignition_link_events(
        &mut self,
        target: Option<u8>,
        transceiver_select: Option<ignition::TransceiverSelect>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ClearIgnitionLinkEvents
        ));
        self.ignition
            .clear_link_events(target, transceiver_select)
            .map_err(sp_error_from_ignition_error)
    }

    fn ignition_command(
        &mut self,
        target: u8,
        command: IgnitionCommand,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::IgnitionCommand {
            target,
            command
        }));
        self.ignition
            .command(target, command)
            .map_err(sp_error_from_ignition_error)
    }

    fn sp_state(&mut self) -> Result<SpStateV2, SpError> {
        let power_state = self.power_state_impl()?;
        self.common.sp_state(power_state)
    }

    fn sp_update_prepare(
        &mut self,
        update: SpUpdatePrepare,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.aux_flash_size + update.sp_image_size,
            component: SpComponent::SP_ITSELF,
            id: update.id,
            slot: 0,
        }));

        self.common.sp_update.prepare(&UPDATE_MEMORY, update)
    }

    fn component_update_prepare(
        &mut self,
        update: ComponentUpdatePrepare,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdatePrepare {
            length: update.total_size,
            component: update.component,
            id: update.id,
            slot: update.slot,
        }));

        match update.component {
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.common.rot_update.prepare(&UPDATE_MEMORY, update)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn component_action(
        &mut self,
        sender: Sender<VLanId>,
        component: SpComponent,
        action: ComponentAction,
    ) -> Result<ComponentActionResponse, SpError> {
        match (component, action) {
            (SpComponent::SYSTEM_LED, ComponentAction::Led(action)) => {
                use gateway_messages::LedComponentAction;
                match action {
                    LedComponentAction::TurnOn => {
                        self.transceivers.set_system_led_on()
                    }
                    LedComponentAction::Blink => {
                        self.transceivers.set_system_led_blink()
                    }
                    LedComponentAction::TurnOff => {
                        self.transceivers.set_system_led_off()
                    }
                }
                .unwrap();
                Ok(ComponentActionResponse::Ack)
            }
            (SpComponent::MONORAIL, ComponentAction::Monorail(action)) => {
                match action {
                    MonorailComponentAction::RequestChallenge => {
                        use drv_sprot_api::{LifecycleState, SpRot};
                        let sprot =
                            SpRot::from(crate::mgs_common::SPROT.get_task_id());
                        let challenge = match sprot.lifecycle_state() {
                            Ok(
                                LifecycleState::Development
                                | LifecycleState::Unprogrammed
                                | LifecycleState::EndOfLife,
                            )
                            | Err(_) => {
                                // Right now, we fail open if we can't talk to
                                // the RoT.  This is intentional: the RoT
                                // protocol has checksum / retries, so we
                                // shouldn't see spurious failures.  If
                                // something has gone sufficiently wrong that we
                                // can't talk to the RoT, then we probably want
                                // to fail into a state where we can debug the
                                // system over the tech port.
                                //
                                // XXX we may want to reevaluate this in the
                                // future!
                                let timestamp = sys_get_timer().now;
                                UnlockChallenge::Trivial { timestamp }
                            }

                            Ok(LifecycleState::Release) => {
                                UnlockChallenge::EcdsaSha2Nistp256(
                                    get_ecdsa_challenge()?,
                                )
                            }
                        };

                        // Store the new challenge, which expires in 60 seconds
                        let now = sys_get_timer().now;
                        self.last_challenge = Some((challenge, now));

                        Ok(ComponentActionResponse::Monorail(
                            MonorailComponentActionResponse::RequestChallenge(
                                challenge,
                            ),
                        ))
                    }
                    MonorailComponentAction::Unlock {
                        challenge,
                        response,
                        time_sec,
                    } => self
                        .unlock(sender.vid, challenge, response, time_sec)
                        .map_err(SpError::Monorail)
                        .map(|()| ComponentActionResponse::Ack),

                    MonorailComponentAction::Lock => {
                        ringbuf_entry!(Trace::ExplicitRelock {
                            vid: sender.vid
                        });
                        self.lock(sender.vid)
                            .map_err(SpError::Monorail)
                            .map(|()| ComponentActionResponse::Ack)
                    }
                }
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_status(
        &mut self,
        component: SpComponent,
    ) -> Result<UpdateStatus, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateStatus {
            component
        }));

        match component {
            SpComponent::SP_ITSELF => Ok(self.common.sp_update.status()),
            SpComponent::ROT | SpComponent::STAGE0 => {
                Ok(self.common.rot_update.status())
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_chunk(
        &mut self,
        chunk: UpdateChunk,
        data: &[u8],
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateChunk {
            component: chunk.component,
            offset: chunk.offset,
        }));

        match chunk.component {
            SpComponent::SP_ITSELF | SpComponent::SP_AUX_FLASH => self
                .common
                .sp_update
                .ingest_chunk(&chunk.component, &chunk.id, chunk.offset, data),
            SpComponent::ROT | SpComponent::STAGE0 => self
                .common
                .rot_update
                .ingest_chunk(&(), &chunk.id, chunk.offset, data),
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn update_abort(
        &mut self,
        component: SpComponent,
        id: UpdateId,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::UpdateAbort {
            component
        }));

        match component {
            SpComponent::SP_ITSELF => self.common.sp_update.abort(&id),
            SpComponent::ROT | SpComponent::STAGE0 => {
                self.common.rot_update.abort(&id)
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn power_state(&mut self) -> Result<PowerState, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetPowerState));
        self.power_state_impl()
    }

    fn set_power_state(
        &mut self,
        sender: Sender<VLanId>,
        power_state: PowerState,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(
            CRITICAL,
            CriticalEvent::SetPowerState {
                sender,
                power_state,
                ticks_since_boot: sys_get_timer().now,
            }
        );
        use drv_sidecar_seq_api::TofinoSequencerPolicy;
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetPowerState(
            power_state
        )));

        let policy = match power_state {
            PowerState::A0 => TofinoSequencerPolicy::LatchOffOnFault,
            PowerState::A2 => TofinoSequencerPolicy::Disabled,
            PowerState::A1 => return Err(SpError::PowerStateError(0)),
        };

        // Sidecar may be in A2 because of a prior sequencing error. There
        // currently is no means for the control plane to learn more about such
        // errors, so simply clear the sequencer before transitioning to A0 in
        // order to avoid getting stuck.
        if power_state == PowerState::A0 {
            self.sequencer
                .clear_tofino_seq_error()
                .map_err(|e| SpError::PowerStateError(e as u32))?
        }

        self.sequencer
            .set_tofino_seq_policy(policy)
            .map_err(|e| SpError::PowerStateError(e as u32))
    }

    fn serial_console_attach(
        &mut self,
        _sender: Sender<VLanId>,
        _component: SpComponent,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleAttach));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_write(
        &mut self,
        _sender: Sender<VLanId>,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleWrite {
            offset,
            length: data.len() as u16
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_keepalive(
        &mut self,
        _sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::SerialConsoleKeepAlive
        ));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_detach(
        &mut self,
        _sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleDetach));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn serial_console_break(
        &mut self,
        _sender: Sender<VLanId>,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SerialConsoleBreak));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn num_devices(&mut self) -> u32 {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::Inventory));
        self.common.inventory().num_devices() as u32
    }

    /// When this method is called by `handle_message`, `index` has been bounds
    /// checked and is guaranteed to be in the range `0..num_devices()`.
    fn device_description(
        &mut self,
        index: BoundsChecked,
    ) -> DeviceDescription<'static> {
        self.common.inventory().device_description(index)
    }

    fn num_component_details(
        &mut self,
        component: SpComponent,
    ) -> Result<u32, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::ComponentDetails {
            component
        }));

        match component {
            SpComponent::MONORAIL => Ok(drv_monorail_api::PORT_COUNT as u32),
            _ => self.common.inventory().num_component_details(&component),
        }
    }

    /// When this method is called by `handle_message`, `index` has been bounds
    /// checked and is guaranteed to be in the range
    /// `0..num_component_details(_, _, component)`.
    fn component_details(
        &mut self,
        component: SpComponent,
        index: BoundsChecked,
    ) -> ComponentDetails {
        match component {
            SpComponent::MONORAIL => ComponentDetails::PortStatus(
                monorail_port_status::port_status(&self.monorail, index),
            ),
            _ => self.common.inventory().component_details(&component, index),
        }
    }

    fn component_get_active_slot(
        &mut self,
        component: SpComponent,
    ) -> Result<u16, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ComponentGetActiveSlot { component }
        ));

        self.common.component_get_active_slot(component)
    }

    fn component_set_active_slot(
        &mut self,
        component: SpComponent,
        slot: u16,
        persist: bool,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ComponentSetActiveSlot {
                component,
                slot,
                persist,
            }
        ));

        self.common
            .component_set_active_slot(component, slot, persist)
    }

    fn component_clear_status(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(
            MgsMessage::ComponentClearStatus { component }
        ));

        // Below we assume we can cast the port count to a u8; const assert that
        // that cast is valid.
        static_assertions::const_assert!(
            drv_monorail_api::PORT_COUNT <= u8::MAX as usize
        );

        match component {
            SpComponent::MONORAIL => {
                // Reset counters on every port.
                for port in 0..drv_monorail_api::PORT_COUNT as u8 {
                    match self.monorail.reset_port_counters(port) {
                        // If `port` is unconfigured, it has no counters to
                        // reset; this isn't a meaningful failure.
                        Ok(()) | Err(MonorailError::UnconfiguredPort) => (),
                        Err(other) => {
                            return Err(SpError::ComponentOperationFailed(
                                other as u32,
                            ));
                        }
                    }
                }
                Ok(())
            }
            _ => Err(SpError::RequestUnsupportedForComponent),
        }
    }

    fn get_startup_options(
        &mut self,
    ) -> Result<gateway_messages::StartupOptions, SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::GetStartupOptions));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn set_startup_options(
        &mut self,
        options: gateway_messages::StartupOptions,
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetStartupOptions(
            options
        )));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn mgs_response_error(&mut self, message_id: u32, err: MgsError) {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::MgsError {
            message_id,
            err
        }));
    }

    fn mgs_response_host_phase2_data(
        &mut self,
        _sender: Sender<VLanId>,
        _message_id: u32,
        hash: [u8; 32],
        offset: u64,
        data: &[u8],
    ) {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::HostPhase2Data {
            hash,
            offset,
            data_len: data.len(),
        }));
    }

    fn send_host_nmi(&mut self) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SendHostNmi));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn set_ipcc_key_lookup_value(
        &mut self,
        key: u8,
        value: &[u8],
    ) -> Result<(), SpError> {
        ringbuf_entry_root!(Log::MgsMessage(MgsMessage::SetIpccKeyValue {
            key,
            value_len: value.len(),
        }));
        Err(SpError::RequestUnsupportedForSp)
    }

    fn get_component_caboose_value(
        &mut self,
        component: SpComponent,
        slot: u16,
        key: [u8; 4],
        buf: &mut [u8],
    ) -> Result<usize, SpError> {
        self.common
            .get_component_caboose_value(component, slot, key, buf)
    }

    fn reset_component_prepare(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        self.common.reset_component_prepare(component)
    }

    fn reset_component_trigger(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        match component {
            SpComponent::MONORAIL => {
                self.common.reset_component_trigger_check(component)?;
                self.monorail
                    .reinit()
                    .map_err(|e| SpError::ComponentOperationFailed(e as u32))
            }
            _ => self.common.reset_component_trigger(component),
        }
    }

    fn read_sensor(
        &mut self,
        req: SensorRequest,
    ) -> Result<SensorResponse, SpError> {
        self.common.read_sensor(req)
    }

    fn current_time(&mut self) -> Result<u64, SpError> {
        self.common.current_time()
    }

    fn read_rot(
        &mut self,
        req: RotRequest,
        buf: &mut [u8],
    ) -> Result<RotResponse, SpError> {
        self.common.read_rot_page(req, buf)
    }

    fn vpd_lock_status_all(
        &mut self,
        buf: &mut [u8],
    ) -> Result<usize, SpError> {
        self.common.vpd_lock_status_all(buf)
    }

    fn reset_component_trigger_with_watchdog(
        &mut self,
        component: SpComponent,
        time_ms: u32,
    ) -> Result<(), SpError> {
        self.common
            .reset_component_trigger_with_watchdog(component, time_ms)
    }

    fn disable_component_watchdog(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        self.common.disable_component_watchdog(component)
    }

    fn component_watchdog_supported(
        &mut self,
        component: SpComponent,
    ) -> Result<(), SpError> {
        self.common.component_watchdog_supported(component)
    }

    fn versioned_rot_boot_info(
        &mut self,
        version: u8,
    ) -> Result<RotBootInfo, SpError> {
        self.common.versioned_rot_boot_info(version)
    }

    fn get_task_dump_count(&mut self) -> Result<u32, SpError> {
        self.common.get_task_dump_count()
    }

    fn task_dump_read_start(
        &mut self,
        index: u32,
        key: [u8; 16],
    ) -> Result<DumpTask, SpError> {
        self.common.task_dump_read_start(index, key)
    }

    fn task_dump_read_continue(
        &mut self,
        key: [u8; 16],
        seq: u32,
        buf: &mut [u8],
    ) -> Result<Option<DumpSegment>, SpError> {
        self.common.task_dump_read_continue(key, seq, buf)
    }
}

// Helper function for `.map_err()`; we can't use `?` because we can't implement
// `From<_>` between these types due to orphan rules.
fn sp_error_from_ignition_error(err: IgnitionError) -> SpError {
    use gateway_messages::ignition::IgnitionError as E;
    let err = match err {
        IgnitionError::FpgaError => E::FpgaError,
        IgnitionError::InvalidPort => E::InvalidPort,
        IgnitionError::InvalidValue => E::InvalidValue,
        IgnitionError::NoTargetPresent => E::NoTargetPresent,
        IgnitionError::RequestInProgress => E::RequestInProgress,
        IgnitionError::RequestDiscarded => E::RequestDiscarded,
        _ => E::Other(err as u32),
    };
    SpError::Ignition(err)
}

fn get_ecdsa_challenge() -> Result<EcdsaSha2Nistp256Challenge, SpError> {
    // Get a nonce from our RNG driver
    let rng = drv_rng_api::Rng::from(RNG.get_task_id());
    let mut nonce = [0u8; 32];
    rng.fill(&mut nonce).map_err(|e| {
        ringbuf_entry!(Trace::RngFillFailed(e));
        SpError::Monorail(GwMonorailError::GetChallengeFailed)
    })?;

    // Get our hardware ID from Packrat
    let packrat = task_packrat_api::Packrat::from(
        crate::mgs_common::PACKRAT.get_task_id(),
    );
    let identity = packrat.get_identity().unwrap_or(VpdIdentity::default());
    const HW_ID_LEN: usize = 32;
    let mut hw_id = [0u8; HW_ID_LEN];
    static_assertions::const_assert!(
        HW_ID_LEN >= core::mem::size_of::<VpdIdentity>()
    );
    hw_id[..core::mem::size_of::<VpdIdentity>()]
        .copy_from_slice(identity.as_bytes());

    let now = sys_get_timer().now;
    Ok(EcdsaSha2Nistp256Challenge {
        hw_id,
        sw_id: [0, 0, 0, 1], // placeholder
        time: now.to_le_bytes(),
        nonce,
    })
}

include!(concat!(env!("OUT_DIR"), "/trusted_keys.rs"));
