// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_ignition_api::{Ignition, IgnitionError};
use drv_monorail_api::MonorailError;
use ringbuf::*;
use userlib::*;
use vsc7448::{Vsc7448, Vsc7448Rw};

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    DisableVsc7448Port(u8),
    EnableVsc7448Port(u8),
    MonorailError(MonorailError),
    IgnitionError(IgnitionError),
    Presence(u64),
}
ringbuf!(Trace, 48, Trace::None);

task_slot!(IGNITION, ignition);

pub struct IgnitionWatcher {
    ignition: Ignition,

    // There are 35 ignition channels, corresponding to
    // - 32 sleds
    // - 2 PSCs
    // - 1 remote Sidecar
    //
    // (the local Sidecar is not available on its own Ignition network)
    //
    // When we boot, the VSC7448 code brings every single port up; we then
    // disable them depending on Ignition presence detect bits.
    enabled: [bool; 35],
}

/// Mapping from Ignition presence bit to VSC7448 port
///
/// This is hard-coded and the same for Sidecar rev A and B
const IGNITION_TO_VSC7448: [u8; 35] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    21, 51, 52, 24, 25, 26, 27, 28, 29, 30, 31, 40, 41, 42,
];

impl IgnitionWatcher {
    pub fn new() -> Self {
        let t: TaskId = IGNITION.get_task_id();
        let ignition = Ignition::from(t);
        Self {
            ignition,
            enabled: [true; 35],
        }
    }

    pub fn wake<R: Vsc7448Rw>(&mut self, vsc7448: &Vsc7448<R>) {
        let presence = match self.ignition.presence_summary() {
            Ok(p) => p,
            Err(e) => {
                ringbuf_entry!(Trace::IgnitionError(e));
                return;
            }
        };

        ringbuf_entry!(Trace::Presence(presence));

        for (i, &port) in IGNITION_TO_VSC7448.iter().enumerate() {
            let now_present = (presence & (1 << i)) != 0;
            let was_present = self.enabled[i];
            if now_present && !was_present {
                ringbuf_entry!(Trace::EnableVsc7448Port(port));
                if let Err(e) = crate::server::reenable_port(port, vsc7448) {
                    ringbuf_entry!(Trace::MonorailError(e));
                }
            } else if was_present && !now_present {
                ringbuf_entry!(Trace::DisableVsc7448Port(port));
                if let Err(e) = crate::server::disable_port(port, vsc7448) {
                    ringbuf_entry!(Trace::MonorailError(e));
                }
            }
            self.enabled[i] = now_present;
        }
    }
}
