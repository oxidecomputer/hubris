// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! API crate for the Ignition controller.

#![no_std]

use core::{array, iter};
use derive_idol_err::IdolError;
use derive_more::From;
use drv_fpga_api::FpgaError;
use idol_runtime::ServerDeath;
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::FromPrimitive;
use serde::Serialize;
use static_assertions::const_assert;
use zerocopy::{AsBytes, FromBytes, Unaligned};

// The `presence_summary` vector (see `drv-ignition-server`) is implicitly
// capped at 40 bits by (the RTL of) the mainboard controller. This constant is
// used to conservatively allocate an array type which can contain the port
// state for all ports. The actual number of ports configured in the system can
// be learned through the `port_count()` function below.
pub const PORT_MAX: u8 = 40;

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    From,
    FromPrimitive,
    ToPrimitive,
    IdolError,
    counters::Count,
)]
pub enum IgnitionError {
    /// Indicates an error communicating with the FPGA implementing the
    /// Controller.
    FpgaError = 1,
    /// Indicates the given port number is larger than the `port_count`.
    InvalidPort,
    /// Indicates an invalid value was provided.
    InvalidValue,
    /// Indicates no Target is present/connected to the port.
    NoTargetPresent,
    /// Indicates the Target is already executing a request. Poll the Target
    /// state after some time and retry if desired.
    RequestInProgress,
    /// Indicates the given request conflicts with the Target system power
    /// state. Poll the Target state and retry if desired.
    RequestDiscarded,

    #[idol(server_death)]
    ServerDied,
}

impl From<ServerDeath> for IgnitionError {
    fn from(_e: ServerDeath) -> Self {
        Self::ServerDied
    }
}

impl From<FpgaError> for IgnitionError {
    fn from(_e: FpgaError) -> Self {
        Self::FpgaError
    }
}

/// `Ignition` aims to provide a more abstracted and stable API for consumers of
/// the Ignition subsystem than the implementation specific data provided by
/// `drv-ignition-server`.
pub struct Ignition {
    controller: idl::Ignition,
}

impl Ignition {
    pub fn new(task_id: userlib::TaskId) -> Self {
        Self {
            controller: idl::Ignition::from(task_id),
        }
    }

    /// Return the number of active Controller ports. This value is expected to
    /// be 35 for production systems but may be smaller in development
    /// environments. See the note above on `PORT_MAX` about the upper bound.
    #[inline]
    pub fn port_count(&self) -> Result<u8, IgnitionError> {
        self.controller.port_count()
    }

    /// Return a u64 with each bit indicating whether or not a Target is present
    /// for this port. See the note above on `PORT_MAX` about the upper bound.
    #[inline]
    pub fn presence_summary(&self) -> Result<u64, IgnitionError> {
        self.controller.presence_summary()
    }

    /// Return the state for the given port.
    #[inline]
    pub fn port(&self, port: u8) -> Result<Port, IgnitionError> {
        self.controller.port_state(port).map(Port::from)
    }

    /// Return the transmitter output enable mode for the given port. See the
    /// type documentation for the different modes available.
    #[inline]
    pub fn transmitter_output_enable_mode(
        &self,
        port: u8,
    ) -> Result<TransmitterOutputEnableMode, IgnitionError> {
        self.controller.transmitter_output_enable_mode(port)
    }

    /// Set the transmitter output enable mode for the given port.
    #[inline]
    pub fn set_transmitter_output_enable_mode(
        &self,
        port: u8,
        mode: TransmitterOutputEnableMode,
    ) -> Result<(), IgnitionError> {
        self.controller
            .set_transmitter_output_enable_mode(port, mode)
    }

    /// Return the `Target` for a given port if present.
    #[inline]
    pub fn target(&self, port: u8) -> Result<Option<Target>, IgnitionError> {
        self.port(port).map(|p| p.target)
    }

    /// Send the given system power `Request` to the given port. Once a request
    /// is sent and accepted by the Target, it enforces a cooldown before
    /// subsequent requests are accepted and processed. `SystemPowerOff` and
    /// `SystemPowerOn` requests have a three second cooldown while a
    /// `SystemPowerReset` has a six seconds cooldown.
    #[inline]
    pub fn send_request(
        &self,
        port: u8,
        request: Request,
    ) -> Result<(), IgnitionError> {
        self.controller.send_request(port, request)
    }

    /// Return the `ApplicationCounters` for the given port. This function has
    /// the side-effect of clearing the counters.
    #[inline]
    pub fn application_counters(
        &self,
        port: u8,
    ) -> Result<ApplicationCounters, IgnitionError> {
        self.controller.application_counters(port)
    }

    /// Return the `TransceiverCounters` for the given port and transceiver. See
    /// `TransceiverSelect` for more details.
    #[inline]
    pub fn transceiver_counters(
        &self,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<TransceiverCounters, IgnitionError> {
        self.controller.transceiver_counters(port, txr)
    }

    /// Return the `TransceiverEvents` for the given port and transceiver. See
    /// `TransceiverSelect` for more details.
    #[inline]
    pub fn transceiver_events(
        &self,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<TransceiverEvents, IgnitionError> {
        self.controller
            .transceiver_events(port, txr)
            .map(TransceiverEvents::from)
    }

    /// Clear the events for the given transceiver and port. See
    /// `TransceiverSelect` for more details.
    #[inline]
    pub fn clear_transceiver_events(
        &self,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<(), IgnitionError> {
        self.controller.clear_transceiver_events(port, txr)
    }

    /// Return the `LinkEvents` for the given port.
    pub fn link_events(&self, port: u8) -> Result<LinkEvents, IgnitionError> {
        self.controller.link_events(port).map(LinkEvents::from)
    }

    /// Fetch the state of all ports in a single operation and return an
    /// iterator over the individual ports. Be aware that this reply is fairly
    /// large and may require enlarging the stack of the caller.
    pub fn all_ports(&self) -> Result<AllPortsIter, IgnitionError> {
        let port_count = usize::from(self.port_count()?);
        let all_port_state = self.controller.all_port_state()?;
        Ok(AllPortsIter {
            iter: all_port_state.into_iter().take(port_count),
        })
    }

    /// Fetch the `LinkEvents` for all ports in a single operation and provide
    /// an iterator over the individual ports.
    pub fn all_link_events(&self) -> Result<AllLinkEventsIter, IgnitionError> {
        let port_count = usize::from(self.port_count()?);
        let all_link_data = self.controller.all_link_events()?;
        Ok(AllLinkEventsIter {
            iter: all_link_data.into_iter().take(port_count),
        })
    }
}

#[derive(Debug)]
pub struct AllPortsIter {
    iter: iter::Take<array::IntoIter<PortState, { PORT_MAX as usize }>>,
}

impl Iterator for AllPortsIter {
    type Item = Port;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(Port::from)
    }
}

#[derive(Debug)]
pub struct AllLinkEventsIter {
    iter: iter::Take<array::IntoIter<[u8; 3], { PORT_MAX as usize }>>,
}

impl Iterator for AllLinkEventsIter {
    type Item = LinkEvents;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(LinkEvents::from)
    }
}

/// `PortState` is an opague type representing (most of) the state of an
/// Ignition Controller port. It is highly dependent on the RTL implementation
/// of the system and the use of the `Port` and `Target` types is encouraged
/// instead.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    FromPrimitive,
    From,
    FromBytes,
    AsBytes,
)]
#[repr(C)]
pub struct PortState(u64);

impl PortState {
    /// A const helper which can be used in static asserts below to check
    /// assumptions about addresses used to access data.
    #[inline]
    const fn byte_offset(addr: Addr) -> usize {
        (addr as usize) - (Addr::TRANSCEIVER_STATE as usize)
    }

    #[inline]
    fn byte(&self, addr: Addr) -> u8 {
        self.0.as_bytes()[Self::byte_offset(addr)]
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Port {
    /// Receiver status of the Controller port.
    pub receiver_status: ReceiverStatus,
    /// Flag indicating whether or not the transmitter output is enabled.
    pub transmitter_output_enabled: bool,
    /// Transmitter output enable mode. See type for details.
    pub transmitter_output_enable_mode: TransmitterOutputEnableMode,
    /// State of the Target, if present. See `Target` for details.
    pub target: Option<Target>,
}

impl From<PortState> for Port {
    fn from(state: PortState) -> Self {
        let target_present = state.byte(Addr::CONTROLLER_STATE)
            & Reg::CONTROLLER_STATE::TARGET_PRESENT
            != 0;

        Self {
            receiver_status: (state.byte(Addr::TRANSCEIVER_STATE) & 0x7).into(),
            transmitter_output_enabled: state.byte(Addr::TRANSCEIVER_STATE)
                & 0x8
                != 0,
            transmitter_output_enable_mode: ((state
                .byte(Addr::TRANSCEIVER_STATE)
                >> 4)
                & 0x3)
                .into(),
            target: if target_present {
                Some(Target::from(state))
            } else {
                None
            },
        }
    }
}

/// `ReceiverStatus` provides high level status bits for the receiver of a link
/// between Controllers and Targets.
#[derive(Copy, Clone, Debug, Default, Serialize)]
pub struct ReceiverStatus {
    /// The receiver has recovered the clock from the transmitter and has
    /// aligned itself with the 8B10B character boundaries in the received data.
    pub aligned: bool,
    /// The receiver is able to succesfully recover the ordered sets (messages)
    /// in the received data.
    pub locked: bool,
    /// The receiver has determined that the P/N polarity of the differential
    /// pair of the link is swapped. The link is operational but a PCB or cable
    /// design change may be desired to correct this condition.
    pub polarity_inverted: bool,
}

impl From<u8> for ReceiverStatus {
    fn from(r: u8) -> ReceiverStatus {
        use Reg::TRANSCEIVER_STATE::*;

        ReceiverStatus {
            aligned: r & RECEIVER_ALIGNED != 0,
            locked: r & RECEIVER_LOCKED != 0,
            polarity_inverted: r & RECEIVER_POLARITY_INVERTED != 0,
        }
    }
}

/// The `TransmitterOutputEnableMode` allow the state of the transmitter output
/// enable of a Controller port to be controlled through software. This can be
/// used when debugging the channel between two systems, is used during loopback
/// testing during manufacturing and attempts to reduce EMI once deployed by
/// keeping a Sidecar from radiating out of open connectors/cubbies.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    From,
    FromPrimitive,
    ToPrimitive,
    AsBytes,
)]
#[repr(u8)]
pub enum TransmitterOutputEnableMode {
    /// The transmitter output is disabled, independent of the receiver state.
    #[default]
    Disabled = 0,
    /// The transmitter output is enabled when the receiver is aligned. This
    /// enables the transmitter at the very first sign of a link peer being
    /// present and requires receiving only a single K28.5 comma character. As a
    /// result this mode is expected to enable the transmitter output even if
    /// the link is (very) marginal or when two Controllers are connected
    /// together.
    EnabledWhenReceiverAligned = 1,
    /// The transmitter output is enabled only when a Target is present. This
    /// mode is more strict and requires several Status messages to be received
    /// by the Controller before it will enable the transmitter output. In this
    /// mode the transmitter will remain disabled if the port is connected to
    /// another Controller.
    EnabledWhenTargetPresent = 2,
    /// The transmitter output is enabled, independent of the receiver state.
    AlwaysEnabled = 3,
}

impl From<TransmitterOutputEnableMode> for u8 {
    fn from(mode: TransmitterOutputEnableMode) -> Self {
        mode as u8
    }
}

impl From<u8> for TransmitterOutputEnableMode {
    fn from(val: u8) -> Self {
        match val {
            1 => TransmitterOutputEnableMode::EnabledWhenReceiverAligned,
            2 => TransmitterOutputEnableMode::EnabledWhenTargetPresent,
            3 => TransmitterOutputEnableMode::AlwaysEnabled,
            _ => TransmitterOutputEnableMode::Disabled,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Serialize)]
pub struct Target {
    /// A numeric id identifying a major type of system. This allows
    /// differentiating between different types of compute, network and power
    /// elements but not different minor revisions of the same systems.
    pub id: SystemId,
    /// The power state of the system controlled by this Target.
    pub power_state: SystemPowerState,
    /// Flag indicating the Target is executing a system power reset.
    pub power_reset_in_progress: bool,
    /// Flags indicating system faults as observed by the Target. The precise
    /// meaning of these may be dependent on the system id.
    pub faults: SystemEvents,
    /// The Target has observed the presence of a Controller on link 0.
    pub controller0_present: bool,
    /// The Target has observed the presence of a Controller on link 1.
    pub controller1_present: bool,
    /// Receiver status of link 0 as reported by the Target.
    pub link0_receiver_status: ReceiverStatus,
    /// Receiver status of link 1 as reported by the Target.
    pub link1_receiver_status: ReceiverStatus,
}

impl Target {
    /// Determine whether or not a system power request is currently in
    /// progress.
    #[inline]
    pub fn request_in_progress(&self) -> bool {
        self.power_reset_in_progress
            || self.power_state == SystemPowerState::PoweringOff
            || self.power_state == SystemPowerState::PoweringOn
    }
}

impl From<PortState> for Target {
    fn from(state: PortState) -> Self {
        use Reg::TARGET_SYSTEM_POWER_REQUEST_STATUS::*;
        use Reg::TARGET_SYSTEM_STATUS::*;

        let system_status = state.byte(Addr::TARGET_SYSTEM_STATUS);
        let system_power_request_status =
            state.byte(Addr::TARGET_SYSTEM_POWER_REQUEST_STATUS);

        Target {
            id: SystemId(state.byte(Addr::TARGET_SYSTEM_TYPE)),
            power_state: SystemPowerState::from_status(
                system_status,
                system_power_request_status,
            ),
            power_reset_in_progress: system_power_request_status
                & POWER_RESET_IN_PROGRESS
                != 0,
            faults: SystemEvents::from(state.byte(Addr::TARGET_SYSTEM_EVENTS)),
            controller0_present: system_status & CONTROLLER0_DETECTED != 0,
            controller1_present: system_status & CONTROLLER1_DETECTED != 0,
            link0_receiver_status: state.byte(Addr::TARGET_LINK0_STATUS).into(),
            link1_receiver_status: state.byte(Addr::TARGET_LINK1_STATUS).into(),
        }
    }
}

/// An enum representing the power state of the system controlled by the Target.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub enum SystemPowerState {
    /// The system is powered down.
    #[default]
    Off,
    /// The system is powered up.
    On,
    /// The system was powered up but encountered a critical power fault and the
    /// Target has disabled system power to avoid damage. A system power request
    /// or press of its power button (if applicable) is needed to clear this
    /// state and transition to the `On` state.
    Aborted,
    /// The system is transitioning from the `On` to the `Off` state.
    PoweringOff,
    /// The system is transitioning from the `Off` to the `On` state.
    PoweringOn,
}

impl SystemPowerState {
    fn from_status(system_status: u8, system_power_request_status: u8) -> Self {
        use Reg::TARGET_SYSTEM_POWER_REQUEST_STATUS::*;
        use Reg::TARGET_SYSTEM_STATUS::*;

        if system_status & SYSTEM_POWER_ABORT != 0 {
            SystemPowerState::Aborted
        } else if system_power_request_status & POWER_ON_IN_PROGRESS != 0 {
            SystemPowerState::PoweringOn
        } else if system_power_request_status & POWER_OFF_IN_PROGRESS != 0 {
            SystemPowerState::PoweringOff
        } else if system_status & SYSTEM_POWER_ENABLED != 0 {
            SystemPowerState::On
        } else {
            SystemPowerState::Off
        }
    }
}

/// `SystemFaults` are faults in a system which may be observed by the Target.
#[derive(Copy, Clone, Debug, Default, Serialize)]
pub struct SystemEvents {
    /// A fault occured with one of the components in the A3 power domain.
    pub power_a3: bool,
    /// A fault occured with one of the components in the A2 power domain.
    pub power_a2: bool,
    /// The RoT was not able to attest the SP and is keeping it from starting.
    pub rot: bool,
    /// The SP was not able to fully boot and/or configure its Ethernet links
    /// with the management network.
    pub sp: bool,
}

impl From<u8> for SystemEvents {
    fn from(r: u8) -> Self {
        use Reg::TARGET_SYSTEM_EVENTS::*;

        Self {
            power_a3: r & POWER_FAULT_A3 != 0,
            power_a2: r & POWER_FAULT_A2 != 0,
            rot: r & ROT != 0,
            sp: r & SP != 0,
        }
    }
}

/// A numeric id identifying a major type of system. This allows differentiating
/// between different types of compute, network and power elements but not
/// different minor revisions of the same systems.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    From,
    FromBytes,
    AsBytes,
    Unaligned,
    Serialize,
)]
#[repr(C)]
pub struct SystemId(pub u8);

/// `Request`s are sent by the Controller to change the power state of a system
/// under control by a Target.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, From, FromPrimitive, ToPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum Request {
    /// Power off the system.
    SystemPowerOff = 1,
    /// Power on the system.
    SystemPowerOn = 2,
    /// Reset the system through a power off followed by a power on transition.
    SystemPowerReset = 3,
}

impl From<Request> for u8 {
    fn from(r: Request) -> Self {
        r as u8
    }
}

/// `Counters` holds several counters collected by the Controller. These are
/// useful to determine if both the Controller and Target are operating correct.
/// The counters will saturate when reaching their maximum value.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, AsBytes, FromBytes, Serialize,
)]
#[repr(C)]
pub struct ApplicationCounters {
    pub target_present: u8,
    pub target_timeout: u8,
    /// The number of Status messages received from the Target.
    pub target_status_received: u8,
    /// The number of Status timeout events observed by the Controller.
    pub target_status_timeout: u8,
    /// The number of Hello messages sent by the Controller.
    pub hello_sent: u8,
    /// The number of system power requests sent by the Controller.
    pub system_power_request_sent: u8,
}

impl ApplicationCounters {
    /// A const helper which can be used in static asserts below to check
    /// assumptions about addresses used to access data.
    #[inline]
    const fn byte_offset(addr: Addr) -> usize {
        (addr as usize) - (Addr::TARGET_PRESENT_COUNT as usize)
    }
}

impl From<[u8; 6]> for ApplicationCounters {
    fn from(data: [u8; 6]) -> Self {
        ApplicationCounters {
            target_present: data[Self::byte_offset(Addr::TARGET_PRESENT_COUNT)],
            target_timeout: data[Self::byte_offset(Addr::TARGET_TIMEOUT_COUNT)],
            target_status_received: data
                [Self::byte_offset(Addr::TARGET_STATUS_RECEIVED_COUNT)],
            target_status_timeout: data
                [Self::byte_offset(Addr::TARGET_STATUS_TIMEOUT_COUNT)],
            hello_sent: data
                [Self::byte_offset(Addr::CONTROLLER_HELLO_SENT_COUNT)],
            system_power_request_sent: data[Self::byte_offset(
                Addr::CONTROLLER_SYSTEM_POWER_REQUEST_SENT_COUNT,
            )],
        }
    }
}

/// `TransceiverEvents` can be used to track some implementation details of an
/// Ignition transceiver. These are sticky and indicate at least one of these
/// events occured since they were cleared.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TransceiverEvents {
    /// The transmitter encoded an invalid 8B10B control character. This should
    /// never occur and is either an indication of a design flaw or corruption
    /// (think bit flip in a LUT) of the logic in the FPGAs implementing the
    /// Controller or Target.
    pub encoding_error: bool,
    /// The receiver received an 8B10B character which was invalid given the
    /// decoder state. This usually indicates bit errors in the received data
    /// These errors are expected to occur during link start-up or when a Target
    /// suddenly goes away due to a loss of power.
    pub decoding_error: bool,
    /// An 8B10B character was received which did not match the expected
    /// character sequence for the expected ordered set. These events may occur
    /// when a Target suddenly goes away due to a loss of power.
    pub ordered_set_invalid: bool,
    /// The version of a received message was invalid.
    pub message_version_invalid: bool,
    /// The type of a received message was invalid. This depends on the system
    /// receiving the message, e.g. this event will occur when a Target receives
    /// a Status message.
    pub message_type_invalid: bool,
    /// The checksum of the message was invalid.
    pub message_checksum_invalid: bool,
}

impl TransceiverEvents {
    pub const NONE: Self = Self::from_u8(0);
    pub const ALL: Self = Self::from_u8(0x3f);

    // Implement as const functions to allow use above.
    const fn from_u8(value: u8) -> Self {
        Self {
            encoding_error: value & 1 << 0 != 0,
            decoding_error: value & 1 << 1 != 0,
            ordered_set_invalid: value & 1 << 2 != 0,
            message_version_invalid: value & 1 << 3 != 0,
            message_type_invalid: value & 1 << 4 != 0,
            message_checksum_invalid: value & 1 << 5 != 0,
        }
    }
}

impl From<u8> for TransceiverEvents {
    fn from(value: u8) -> Self {
        Self::from_u8(value)
    }
}

impl From<TransceiverCounters> for TransceiverEvents {
    fn from(counters: TransceiverCounters) -> Self {
        Self {
            encoding_error: counters.encoding_error != 0,
            decoding_error: counters.decoding_error != 0,
            ordered_set_invalid: counters.ordered_set_invalid != 0,
            message_version_invalid: counters.message_version_invalid != 0,
            message_type_invalid: counters.message_type_invalid != 0,
            message_checksum_invalid: counters.message_checksum_invalid != 0,
        }
    }
}

impl From<TransceiverEvents> for u8 {
    fn from(events: TransceiverEvents) -> u8 {
        0u8 | if events.encoding_error { 1 << 0 } else { 0 }
            | if events.decoding_error { 1 << 1 } else { 0 }
            | if events.ordered_set_invalid {
                1 << 2
            } else {
                0
            }
            | if events.message_version_invalid {
                1 << 3
            } else {
                0
            }
            | if events.message_type_invalid {
                1 << 4
            } else {
                0
            }
            | if events.message_checksum_invalid {
                1 << 5
            } else {
                0
            }
    }
}

/// `TransceiverEvents` can be used to track some implementation details of an
/// Ignition transceiver. These are sticky and indicate at least one of these
/// events occured since they were cleared.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, AsBytes, FromBytes, Serialize,
)]
#[repr(C)]
pub struct TransceiverCounters {
    pub receiver_reset: u8,
    pub receiver_aligned: u8,
    pub receiver_locked: u8,
    pub receiver_polarity_inverted: u8,
    /// The transmitter encoded an invalid 8B10B control character. This should
    /// never occur and is either an indication of a design flaw or corruption
    /// (think bit flip in a LUT) of the logic in the FPGAs implementing the
    /// Controller or Target.
    pub encoding_error: u8,
    /// The receiver received an 8B10B character which was invalid given the
    /// decoder state. This usually indicates bit errors in the received data
    /// These errors are expected to occur during link start-up or when a Target
    /// suddenly goes away due to a loss of power.
    pub decoding_error: u8,
    /// An 8B10B character was received which did not match the expected
    /// character sequence for the expected ordered set. These events may occur
    /// when a Target suddenly goes away due to a loss of power.
    pub ordered_set_invalid: u8,
    /// The version of a received message was invalid.
    pub message_version_invalid: u8,
    /// The type of a received message was invalid. This depends on the system
    /// receiving the message, e.g. this event will occur when a Target receives
    /// a Status message.
    pub message_type_invalid: u8,
    /// The checksum of the message was invalid.
    pub message_checksum_invalid: u8,
}

impl TransceiverCounters {
    /// A const helper which can be used in static asserts below to check
    /// assumptions about addresses used to access data.
    #[inline]
    const fn byte_offset(base: Addr, addr: Addr) -> usize {
        (addr as usize) - (base as usize)
    }

    pub fn from_controller(data: [u8; 10]) -> Self {
        let base = Addr::CONTROLLER_RECEIVER_RESET_COUNT;

        Self {
            receiver_reset: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_RECEIVER_RESET_COUNT,
            )],
            receiver_aligned: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_RECEIVER_ALIGNED_COUNT,
            )],
            receiver_locked: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_RECEIVER_LOCKED_COUNT,
            )],
            receiver_polarity_inverted: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_RECEIVER_POLARITY_INVERTED_COUNT,
            )],
            encoding_error: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_ENCODING_ERROR_COUNT,
            )],
            decoding_error: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_DECODING_ERROR_COUNT,
            )],
            ordered_set_invalid: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_ORDERED_SET_INVALID_COUNT,
            )],
            message_version_invalid: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_MESSAGE_VERSION_INVALID_COUNT,
            )],
            message_type_invalid: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_MESSAGE_TYPE_INVALID_COUNT,
            )],
            message_checksum_invalid: data[Self::byte_offset(
                base,
                Addr::CONTROLLER_MESSAGE_CHECKSUM_INVALID_COUNT,
            )],
        }
    }

    pub fn from_target_link0(data: [u8; 10]) -> Self {
        let base = Addr::TARGET_LINK0_RECEIVER_RESET_COUNT;

        Self {
            receiver_reset: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
            )],
            receiver_aligned: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_RECEIVER_ALIGNED_COUNT,
            )],
            receiver_locked: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_RECEIVER_LOCKED_COUNT,
            )],
            receiver_polarity_inverted: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_RECEIVER_POLARITY_INVERTED_COUNT,
            )],
            encoding_error: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_ENCODING_ERROR_COUNT,
            )],
            decoding_error: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_DECODING_ERROR_COUNT,
            )],
            ordered_set_invalid: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_ORDERED_SET_INVALID_COUNT,
            )],
            message_version_invalid: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_MESSAGE_VERSION_INVALID_COUNT,
            )],
            message_type_invalid: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_MESSAGE_TYPE_INVALID_COUNT,
            )],
            message_checksum_invalid: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK0_MESSAGE_CHECKSUM_INVALID_COUNT,
            )],
        }
    }

    pub fn from_target_link1(data: [u8; 10]) -> Self {
        let base = Addr::TARGET_LINK1_RECEIVER_RESET_COUNT;

        Self {
            receiver_reset: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
            )],
            receiver_aligned: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_RECEIVER_ALIGNED_COUNT,
            )],
            receiver_locked: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_RECEIVER_LOCKED_COUNT,
            )],
            receiver_polarity_inverted: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_RECEIVER_POLARITY_INVERTED_COUNT,
            )],
            encoding_error: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_ENCODING_ERROR_COUNT,
            )],
            decoding_error: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_DECODING_ERROR_COUNT,
            )],
            ordered_set_invalid: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_ORDERED_SET_INVALID_COUNT,
            )],
            message_version_invalid: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_MESSAGE_VERSION_INVALID_COUNT,
            )],
            message_type_invalid: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_MESSAGE_TYPE_INVALID_COUNT,
            )],
            message_checksum_invalid: data[Self::byte_offset(
                base,
                Addr::TARGET_LINK1_MESSAGE_CHECKSUM_INVALID_COUNT,
            )],
        }
    }
}

/// Transceiver events are observed by a transceiver, therefor each link between
/// a Controller and Target has two sets of `TransceiverEvents`. The Target
/// notifies both Controllers when events are observed by either of its
/// transceivers. As a result each Controller has visibility into and keeps
/// track of three sets of these events; its own tranceiver to the Target and
/// both transceivers of the Target. When operating on `TransceiverEvents` this
/// enum is used to select between the different sets.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, From, FromPrimitive, ToPrimitive, AsBytes,
)]
#[repr(u8)]
pub enum TransceiverSelect {
    Controller = 1,
    TargetLink0 = 2,
    TargetLink1 = 3,
}

impl TransceiverSelect {
    /// Convenience set of all transmitters used in some of the batch operations
    /// on `LinkEvents`.
    pub const ALL: [Self; 3] =
        [Self::Controller, Self::TargetLink0, Self::TargetLink1];
}

/// `LinkEvents` is a convenience type to represent the complete set of
/// transceiver events observed by a Controller port.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LinkEvents {
    pub controller: TransceiverEvents,
    pub target_link0: TransceiverEvents,
    pub target_link1: TransceiverEvents,
}

impl From<[u8; 3]> for LinkEvents {
    fn from(r: [u8; 3]) -> Self {
        Self {
            controller: TransceiverEvents::from(r[0]),
            target_link0: TransceiverEvents::from(r[1]),
            target_link1: TransceiverEvents::from(r[2]),
        }
    }
}

/// A flattened struct representing the state of a port which can be
/// reconstructed by Humility from a ssmarshal encoded buffer using DWARF
/// information.
#[derive(Copy, Clone, Debug, Default, Serialize)]
pub struct IgnitionPortStateForHumility {
    pub target_present: bool,
    pub target: Target,
    pub receiver_status: ReceiverStatus,
    pub application_counters: ApplicationCounters,
    pub link_events: LinkEvents,
}

impl From<Port> for IgnitionPortStateForHumility {
    fn from(port: Port) -> Self {
        Self {
            target_present: port.target.is_some(),
            target: port.target.unwrap_or_default(),
            receiver_status: port.receiver_status,
            // The remaining fields require additional data so use defaults.
            application_counters: Default::default(),
            link_events: Default::default(),
        }
    }
}

pub use reg_map::Addr;
pub use reg_map::Reg;

mod reg_map {
    include!(concat!(env!("OUT_DIR"), "/ignition_controller.rs"));

    impl From<Addr> for usize {
        fn from(addr: Addr) -> Self {
            addr as usize
        }
    }
}

/// `PortState` is a linear representation of several registers in the
/// Controller register page. The generated addresses for these registers can be
/// used to lookup data but assumptions about them should be validated.
use core::mem::size_of;

const_assert!(
    PortState::byte_offset(Addr::TRANSCEIVER_STATE) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::CONTROLLER_STATE) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_SYSTEM_TYPE) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_SYSTEM_STATUS) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_SYSTEM_EVENTS) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_SYSTEM_POWER_REQUEST_STATUS)
        < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_LINK0_STATUS) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_LINK1_STATUS) < size_of::<PortState>()
);

// Check assumptions about register addresses for counters.
const_assert!(ApplicationCounters::byte_offset(Addr::TARGET_PRESENT_COUNT) < 6);
const_assert!(ApplicationCounters::byte_offset(Addr::TARGET_TIMEOUT_COUNT) < 6);
const_assert!(
    ApplicationCounters::byte_offset(Addr::TARGET_STATUS_RECEIVED_COUNT) < 6
);
const_assert!(
    ApplicationCounters::byte_offset(Addr::TARGET_STATUS_TIMEOUT_COUNT) < 6
);
const_assert!(
    ApplicationCounters::byte_offset(Addr::CONTROLLER_HELLO_SENT_COUNT) < 6
);
const_assert!(
    ApplicationCounters::byte_offset(
        Addr::CONTROLLER_SYSTEM_POWER_REQUEST_SENT_COUNT
    ) < 6
);

const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_RECEIVER_RESET_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_RECEIVER_ALIGNED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_RECEIVER_LOCKED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_RECEIVER_POLARITY_INVERTED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_ENCODING_ERROR_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_DECODING_ERROR_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_ORDERED_SET_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_MESSAGE_VERSION_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_MESSAGE_TYPE_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::CONTROLLER_RECEIVER_RESET_COUNT,
        Addr::CONTROLLER_MESSAGE_CHECKSUM_INVALID_COUNT
    ) < 10
);

const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_RECEIVER_ALIGNED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_RECEIVER_LOCKED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_RECEIVER_POLARITY_INVERTED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_ENCODING_ERROR_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_DECODING_ERROR_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_ORDERED_SET_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_MESSAGE_VERSION_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_MESSAGE_TYPE_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK0_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK0_MESSAGE_CHECKSUM_INVALID_COUNT
    ) < 10
);

const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_RECEIVER_ALIGNED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_RECEIVER_LOCKED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_RECEIVER_POLARITY_INVERTED_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_ENCODING_ERROR_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_DECODING_ERROR_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_ORDERED_SET_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_MESSAGE_VERSION_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_MESSAGE_TYPE_INVALID_COUNT
    ) < 10
);
const_assert!(
    TransceiverCounters::byte_offset(
        Addr::TARGET_LINK1_RECEIVER_RESET_COUNT,
        Addr::TARGET_LINK1_MESSAGE_CHECKSUM_INVALID_COUNT
    ) < 10
);

mod idl {
    use crate as drv_ignition_api;
    use userlib::sys_send;

    include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
}
