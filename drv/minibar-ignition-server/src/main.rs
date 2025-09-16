// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for interacting with Minibar's Ignition Controllers.

#![no_std]
#![no_main]

use drv_ignition_api::*;
use drv_minibar_seq_api::Sequencer;
use ringbuf::*;
use userlib::{hl, sys_get_timer, sys_set_timer, task_slot};

mod ignition;

task_slot!(FPGA, fpga);
task_slot!(SEQUENCER, sequencer);

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    AwaitingControllerReady,
    PortCount(u8),
    PresenceUpdate(u8),
    PresencePollError(IgnitionError),
    TargetError(u8, IgnitionError),
    TargetArrive(u8),
    TargetDepart(u8),
    SystemPowerRequest(u8, Request),
    SystemPowerRequestError(u8, IgnitionError),
}
ringbuf!(Trace, 16, Trace::None);

const TIMER_INTERVAL: u64 = 1000;

struct ServerImpl {
    controller: ignition::IgnitionController,
    port_count: u8,
    last_presence_summary: u8,
}

#[export_name = "main"]
fn main() -> ! {
    let mut incoming = [0u8; idl::INCOMING_SIZE];
    let mut server = ServerImpl {
        controller: ignition::IgnitionController::new(FPGA.get_task_id()),
        port_count: 0,
        last_presence_summary: 0,
    };

    let sequencer = Sequencer::from(SEQUENCER.get_task_id());

    // Poll the sequencer to determine if the mainboard controller is
    // ready.
    ringbuf_entry!(Trace::AwaitingControllerReady);
    while !sequencer.controller_ready().unwrap_or(false) {
        hl::sleep_for(25);
    }

    // Determine the number of Ignition controllers available.
    server.port_count = server.controller.port_count();
    ringbuf_entry!(Trace::PortCount(server.port_count));

    // Set a timer in the past causing the presence state to be polled and
    // updated as soon as the serving loop starts.
    sys_set_timer(Some(sys_get_timer().now), notifications::TIMER_MASK);

    loop {
        idol_runtime::dispatch(&mut incoming, &mut server);
    }
}

impl ServerImpl {
    /// Get the state of the given Target or an error if no Target present.
    fn target(&self, port: u8) -> Result<Target, IgnitionError> {
        Port::from(
            self.controller
                .port_state(port)
                .map_err(IgnitionError::from)?,
        )
        .target
        .ok_or(IgnitionError::NoTargetPresent)
    }

    /// Poll the presence summary and track Targets arriving and departing.
    fn poll_presence(&mut self) -> Result<(), IgnitionError> {
        let current_presence_summary = self.controller.presence_summary()?;

        if current_presence_summary != self.last_presence_summary {
            let arriving_targets =
                current_presence_summary & !self.last_presence_summary;
            let departing_targets =
                !current_presence_summary & self.last_presence_summary;

            let arrived_targets = self
                .map_ports(arriving_targets, |port| self.target_arrive(port));
            let departed_targets = self
                .map_ports(departing_targets, |port| self.target_depart(port));

            // Update the presence summary based on targets which were
            // succesfully processed. If a target wasn't processed it'll get
            // retried on the next cycle.
            self.last_presence_summary = arrived_targets
                | (self.last_presence_summary & !departed_targets);

            ringbuf_entry!(Trace::PresenceUpdate(self.last_presence_summary));
        }

        Ok(())
    }

    /// Apply the given function to each port for which a bit in the `ports`
    /// vector is set. Returns a bit vector with bits set for ports for which
    /// the operation was succesful. Under normal circumstances this output
    /// vector is expected to match the input vector.
    fn map_ports<F>(&self, ports: u8, mut f: F) -> u8
    where
        F: FnMut(u8) -> Result<(), IgnitionError>,
    {
        let mut success = 0u8;

        for port in 0..self.port_count.min(PORT_MAX) {
            let mask = 1 << port;

            if ports & mask != 0 {
                match f(port) {
                    Ok(()) => success |= mask,
                    Err(e) => ringbuf_entry!(Trace::TargetError(port, e)),
                }
            }
        }

        success
    }

    /// Callback which gets called whenever a Target is first seen.
    fn target_arrive(&self, port: u8) -> Result<(), IgnitionError> {
        ringbuf_entry!(Trace::TargetArrive(port));

        // Clear counters.
        self.controller.counters(port)?;

        // Reset the events for each transceiver if the register is set to its
        // default value.
        for txr in &TransceiverSelect::ALL {
            let events = TransceiverEvents::from(
                self.controller
                    .transceiver_events(port, *txr)
                    .map_err(IgnitionError::from)?,
            );

            if events == TransceiverEvents::ALL {
                self.controller
                    .clear_transceiver_events(port, *txr)
                    .map_err(IgnitionError::from)?;
            }
        }

        Ok(())
    }

    /// Callback which gets called whenever a Target goes away.
    fn target_depart(&self, port: u8) -> Result<(), IgnitionError> {
        ringbuf_entry!(Trace::TargetDepart(port));
        Ok(())
    }

    fn target_request(
        &self,
        port: u8,
        request: Request,
    ) -> Result<(), IgnitionError> {
        if self.target(port)?.request_in_progress() {
            return Err(IgnitionError::RequestInProgress);
        }

        // Port 35 is connected to the local Target. Allowing a Controller to
        // send a SystemPowerReset to this port, effectively power resetting
        // itself, can make sense under some circumstances (e.g. autonomously
        // updating VR configuration). But to avoid someone or something
        // accidentally sending a SystemPowerOff request and potentially
        // powering off the system until power to the bus bar is cycled any
        // request other than a SystemPowerReset is rejected.
        if port == 35 && request != Request::SystemPowerReset {
            return Err(IgnitionError::RequestDiscarded);
        }

        self.controller
            .set_request(port, request)
            .map_err(IgnitionError::from)?;

        // Determine if the request was accepted by matching the (updated)
        // Target power state with the request.
        match (request, self.target(port)?.power_state) {
            (
                Request::SystemPowerOff,
                SystemPowerState::PoweringOff | SystemPowerState::Off,
            )
            | (
                Request::SystemPowerOn,
                SystemPowerState::PoweringOn | SystemPowerState::On,
            )
            | (
                Request::SystemPowerReset,
                SystemPowerState::PoweringOff | SystemPowerState::PoweringOn,
            ) => Ok(()),
            _ => Err(IgnitionError::RequestDiscarded),
        }
    }
}

type RequestError = idol_runtime::RequestError<IgnitionError>;

impl idl::InOrderIgnitionImpl for ServerImpl {
    fn port_count(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u8, RequestError> {
        if self.port_count == 0xff {
            Err(RequestError::from(IgnitionError::FpgaError))
        } else {
            Ok(self.port_count)
        }
    }

    fn presence_summary(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<u64, RequestError> {
        Ok(u64::from(self.last_presence_summary))
    }

    fn port_state(
        &mut self,
        _: &userlib::RecvMessage,
        port: u8,
    ) -> Result<PortState, RequestError> {
        if port >= self.port_count {
            return Err(RequestError::from(IgnitionError::InvalidPort));
        }

        self.controller
            .port_state(port)
            .map_err(IgnitionError::from)
            .map_err(RequestError::from)
    }

    fn always_transmit(
        &mut self,
        _: &userlib::RecvMessage,
        port: u8,
    ) -> Result<bool, RequestError> {
        if port >= self.port_count {
            return Err(RequestError::from(IgnitionError::InvalidPort));
        }

        self.controller
            .always_transmit(port)
            .map_err(IgnitionError::from)
            .map_err(RequestError::from)
    }

    fn set_always_transmit(
        &mut self,
        _: &userlib::RecvMessage,
        port: u8,
        enabled: bool,
    ) -> Result<(), RequestError> {
        if port >= self.port_count {
            return Err(RequestError::from(IgnitionError::InvalidPort));
        }

        self.controller
            .set_always_transmit(port, enabled)
            .map_err(IgnitionError::from)
            .map_err(RequestError::from)
    }

    fn counters(
        &mut self,
        _: &userlib::RecvMessage,
        port: u8,
    ) -> Result<Counters, RequestError> {
        if port >= self.port_count {
            return Err(RequestError::from(IgnitionError::InvalidPort));
        }

        self.controller
            .counters(port)
            .map_err(IgnitionError::from)
            .map_err(RequestError::from)
    }

    fn transceiver_events(
        &mut self,
        _: &userlib::RecvMessage,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<u8, RequestError> {
        if port >= self.port_count {
            return Err(RequestError::from(IgnitionError::InvalidPort));
        }

        self.controller
            .transceiver_events(port, txr)
            .map_err(IgnitionError::from)
            .map_err(RequestError::from)
    }

    fn clear_transceiver_events(
        &mut self,
        _: &userlib::RecvMessage,
        port: u8,
        txr: TransceiverSelect,
    ) -> Result<(), RequestError> {
        if port >= self.port_count {
            return Err(RequestError::from(IgnitionError::InvalidPort));
        }

        self.controller
            .clear_transceiver_events(port, txr)
            .map_err(IgnitionError::from)
            .map_err(RequestError::from)
    }

    fn link_events(
        &mut self,
        _: &userlib::RecvMessage,
        port: u8,
    ) -> Result<[u8; 3], RequestError> {
        if port >= self.port_count {
            return Err(RequestError::from(IgnitionError::InvalidPort));
        }

        let mut events = [0u8; 3];
        for (i, txr) in TransceiverSelect::ALL.into_iter().enumerate() {
            events[i] = self
                .controller
                .transceiver_events(port, txr)
                .map_err(IgnitionError::from)?;
        }

        Ok(events)
    }

    fn send_request(
        &mut self,
        _: &userlib::RecvMessage,
        port: u8,
        request: Request,
    ) -> Result<(), RequestError> {
        if port >= self.port_count {
            return Err(RequestError::from(IgnitionError::InvalidPort));
        }

        ringbuf_entry!(Trace::SystemPowerRequest(port, request));

        self.target_request(port, request).map_err(|e| {
            ringbuf_entry!(Trace::SystemPowerRequestError(port, e));
            RequestError::from(e)
        })
    }

    fn all_port_state(
        &mut self,
        _: &userlib::RecvMessage,
    ) -> Result<[PortState; PORT_MAX as usize], RequestError> {
        let mut state = [Default::default(); PORT_MAX as usize];

        for port in 0..PORT_MAX.min(self.port_count) {
            state[port as usize] = self
                .controller
                .port_state(port)
                .map_err(IgnitionError::from)?;
        }

        Ok(state)
    }

    fn all_link_events(
        &mut self,
        msg: &userlib::RecvMessage,
    ) -> Result<[[u8; 3]; PORT_MAX as usize], RequestError> {
        let mut all_link_events = [[0u8; 3]; PORT_MAX as usize];

        for port in 0..PORT_MAX.min(self.port_count) {
            all_link_events[port as usize] = self.link_events(msg, port)?;
        }

        Ok(all_link_events)
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, _bits: u32) {
        if sys_get_timer().deadline.is_some() {
            return;
        }

        let start = sys_get_timer().now;

        // Only poll the presence summary if the port count seems reasonable. A
        // count of 0xff may occur if the FPGA is running an incorrect
        // bitstream.
        if self.port_count > 0 && self.port_count != 0xff {
            if let Err(e) = self.poll_presence() {
                ringbuf_entry!(Trace::PresencePollError(e));
            }
        }

        let finish = sys_get_timer().now;

        // We now know when we were notified and when any work was completed.
        // Note that the assumption here is that `start` < `finish` and that
        // this won't hold if the system time rolls over. But, the system timer
        // is a u64, with each bit representing a ms, so in practice this should
        // be fine. Anyway, armed with this information, find the next deadline
        // some multiple of `TIMER_INTERVAL` in the future.

        let delta = finish - start;
        let next_deadline = finish + TIMER_INTERVAL - (delta % TIMER_INTERVAL);

        sys_set_timer(Some(next_deadline), notifications::TIMER_MASK);
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
