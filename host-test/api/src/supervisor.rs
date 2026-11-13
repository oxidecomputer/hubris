// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Control interface of `host-test-supervisor`.

use serde::{Deserialize, Serialize};
use userlib::TaskId;

use crate::control::call;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    /// robo: Ends the run successfully (the kernel exits 0).
    Pass,
    /// robo: Restarts the task at `index`, as a supervisor would after a
    /// robo: crash.
    RestartTask { index: u16 },
}

/// robo: Client for the supervisor's control interface.
#[derive(Copy, Clone)]
pub struct Supervisor(pub TaskId);

impl Supervisor {
    pub fn pass(&self) -> ! {
        let () = call(self.0, &Request::Pass);
        unreachable!("the supervisor returned from a system restart");
    }

    pub fn restart_task(&self, index: u16) {
        call::<_, ()>(self.0, &Request::RestartTask { index })
    }
}
