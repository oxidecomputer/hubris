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
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

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

    /// Return whether or not the port will always transmit even if no Target is
    /// present.
    #[inline]
    pub fn always_transmit(&self, port: u8) -> Result<bool, IgnitionError> {
        self.controller.always_transmit(port)
    }

    /// Set whether or not the port will always transmit even if no Target is
    /// present.
    #[inline]
    pub fn set_always_transmit(
        &self,
        port: u8,
        enabled: bool,
    ) -> Result<(), IgnitionError> {
        self.controller.set_always_transmit(port, enabled)
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

    /// Return the `Counters` for the given port. This function has the
    /// side-effect of clearing the counters.
    #[inline]
    pub fn counters(&self, port: u8) -> Result<Counters, IgnitionError> {
        self.controller.counters(port)
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
    IntoBytes,
    Immutable,
    KnownLayout,
)]
#[repr(C)]
pub struct PortState(u64);

impl PortState {
    /// A const helper which can be used in static asserts below to check
    /// assumptions about addresses used to access data.
    #[inline]
    const fn byte_offset(addr: Addr) -> usize {
        (addr as usize) - (Addr::CONTROLLER_STATE as usize)
    }

    #[inline]
    fn byte(&self, addr: Addr) -> u8 {
        self.0.as_bytes()[Self::byte_offset(addr)]
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Port {
    /// The port is configured to transmit irrespective of Target presence.
    pub always_transmit: bool,
    /// Receiver status of the Controller port.
    pub receiver_status: ReceiverStatus,
    /// State of the Target, if present. See `Target` for details.
    pub target: Option<Target>,
}

impl From<PortState> for Port {
    fn from(state: PortState) -> Self {
        let target_present = state.byte(Addr::CONTROLLER_STATE)
            & Reg::CONTROLLER_STATE::TARGET_PRESENT
            != 0;

        Self {
            always_transmit: state.byte(Addr::CONTROLLER_STATE)
                & Reg::CONTROLLER_STATE::ALWAYS_TRANSMIT
                != 0,
            receiver_status: state.byte(Addr::CONTROLLER_LINK_STATUS).into(),
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
        use Reg::CONTROLLER_LINK_STATUS::*;

        ReceiverStatus {
            aligned: r & RECEIVER_ALIGNED != 0,
            locked: r & RECEIVER_LOCKED != 0,
            polarity_inverted: r & POLARITY_INVERTED != 0,
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
    pub faults: SystemFaults,
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
        use Reg::TARGET_REQUEST_STATUS::*;
        use Reg::TARGET_SYSTEM_STATUS::*;

        let system_status = state.byte(Addr::TARGET_SYSTEM_STATUS);
        let request_status = state.byte(Addr::TARGET_REQUEST_STATUS);

        Target {
            id: SystemId(state.byte(Addr::TARGET_SYSTEM_TYPE)),
            power_state: SystemPowerState::from((
                system_status,
                request_status,
            )),
            power_reset_in_progress: request_status & SYSTEM_RESET_IN_PROGRESS
                != 0,
            faults: SystemFaults::from(state.byte(Addr::TARGET_SYSTEM_FAULTS)),
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

impl From<(u8, u8)> for SystemPowerState {
    fn from(state: (u8, u8)) -> Self {
        use Reg::TARGET_REQUEST_STATUS::*;
        use Reg::TARGET_SYSTEM_STATUS::*;

        let (system_status, request_status) = state;

        if system_status & SYSTEM_POWER_ABORT != 0 {
            SystemPowerState::Aborted
        } else if request_status & POWER_ON_IN_PROGRESS != 0 {
            SystemPowerState::PoweringOn
        } else if request_status & POWER_OFF_IN_PROGRESS != 0 {
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
pub struct SystemFaults {
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

impl From<u8> for SystemFaults {
    fn from(r: u8) -> Self {
        use Reg::TARGET_SYSTEM_FAULTS::*;

        Self {
            power_a3: r & POWER_FAULT_A3 != 0,
            power_a2: r & POWER_FAULT_A2 != 0,
            rot: r & ROT_FAULT != 0,
            sp: r & SP_FAULT != 0,
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
    IntoBytes,
    Unaligned,
    Serialize,
    Immutable,
    KnownLayout,
)]
#[repr(C)]
pub struct SystemId(pub u8);

/// `Request`s are sent by the Controller to change the power state of a system
/// under control by a Target.
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    From,
    FromPrimitive,
    ToPrimitive,
    IntoBytes,
    Immutable,
    KnownLayout,
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
    Copy, Clone, Debug, Default, PartialEq, Eq, IntoBytes, FromBytes, Serialize,
)]
#[repr(C)]
pub struct Counters {
    /// The number of Status messages received from the Target.
    pub status_received: u8,
    /// The number of Hello messages sent by the Controller.
    pub hello_sent: u8,
    /// The number of requests sent by the Controller.
    pub request_sent: u8,
    /// The number of Hello or Request messages dropped by the Controller. A
    /// Target does not send these messages thus this counter is expected to
    /// always be zero.
    pub message_dropped: u8,
}

impl Counters {
    /// A const helper which can be used in static asserts below to check
    /// assumptions about addresses used to access data.
    #[inline]
    const fn byte_offset(addr: Addr) -> usize {
        (addr as usize) - (Addr::CONTROLLER_STATUS_RECEIVED_COUNT as usize)
    }
}

impl From<[u8; 4]> for Counters {
    fn from(data: [u8; 4]) -> Self {
        Counters {
            status_received: data
                [Self::byte_offset(Addr::CONTROLLER_STATUS_RECEIVED_COUNT)],
            hello_sent: data
                [Self::byte_offset(Addr::CONTROLLER_HELLO_SENT_COUNT)],
            request_sent: data
                [Self::byte_offset(Addr::CONTROLLER_REQUEST_SENT_COUNT)],
            message_dropped: data
                [Self::byte_offset(Addr::CONTROLLER_MESSAGE_DROPPED_COUNT)],
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
    const fn from_u8(r: u8) -> Self {
        use Reg::CONTROLLER_LINK_EVENTS_SUMMARY::*;

        Self {
            encoding_error: r & ENCODING_ERROR != 0,
            decoding_error: r & DECODING_ERROR != 0,
            ordered_set_invalid: r & ORDERED_SET_INVALID != 0,
            message_version_invalid: r & MESSAGE_VERSION_INVALID != 0,
            message_type_invalid: r & MESSAGE_TYPE_INVALID != 0,
            message_checksum_invalid: r & MESSAGE_CHECKSUM_INVALID != 0,
        }
    }
}

impl From<u8> for TransceiverEvents {
    fn from(reg: u8) -> Self {
        Self::from_u8(reg)
    }
}

impl From<TransceiverEvents> for u8 {
    fn from(events: TransceiverEvents) -> u8 {
        use Reg::CONTROLLER_LINK_EVENTS_SUMMARY::*;

        0u8 | if events.encoding_error {
            ENCODING_ERROR
        } else {
            0
        } | if events.decoding_error {
            DECODING_ERROR
        } else {
            0
        } | if events.ordered_set_invalid {
            ORDERED_SET_INVALID
        } else {
            0
        } | if events.message_version_invalid {
            MESSAGE_VERSION_INVALID
        } else {
            0
        } | if events.message_type_invalid {
            MESSAGE_TYPE_INVALID
        } else {
            0
        } | if events.message_checksum_invalid {
            MESSAGE_CHECKSUM_INVALID
        } else {
            0
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
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    From,
    FromPrimitive,
    ToPrimitive,
    IntoBytes,
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
    pub counters: Counters,
    pub link_events: LinkEvents,
}

impl From<Port> for IgnitionPortStateForHumility {
    fn from(port: Port) -> Self {
        Self {
            target_present: port.target.is_some(),
            target: port.target.unwrap_or_default(),
            receiver_status: port.receiver_status,
            // The remaining fields require additional data so use defaults.
            counters: Default::default(),
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
    PortState::byte_offset(Addr::CONTROLLER_STATE) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::CONTROLLER_LINK_STATUS)
        < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_SYSTEM_TYPE) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_SYSTEM_STATUS) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_SYSTEM_FAULTS) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_REQUEST_STATUS)
        < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_LINK0_STATUS) < size_of::<PortState>()
);
const_assert!(
    PortState::byte_offset(Addr::TARGET_LINK1_STATUS) < size_of::<PortState>()
);

// Check assumptions about register addresses for counters.
const_assert!(
    Counters::byte_offset(Addr::CONTROLLER_STATUS_RECEIVED_COUNT) < 4
);
const_assert!(Counters::byte_offset(Addr::CONTROLLER_HELLO_SENT_COUNT) < 4);
const_assert!(Counters::byte_offset(Addr::CONTROLLER_REQUEST_SENT_COUNT) < 4);
const_assert!(
    Counters::byte_offset(Addr::CONTROLLER_MESSAGE_DROPPED_COUNT) < 4
);

mod idl {
    use crate as drv_ignition_api;
    use userlib::sys_send;

    include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
}
