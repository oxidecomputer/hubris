// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Shared pieces of the host integration test fixtures.
//! robo:
//! robo: A host test image has a supervisor, the task under test, fake
//! robo: servers standing in for the hardware-facing tasks it calls, and an
//! robo: orchestrator that runs one scenario. The orchestrator drives and
//! robo: inspects the fakes through a control operation ([`control`]) that
//! robo: each fake serves alongside the real API it stands in for.
//! robo:
//! robo: Everything here only ever runs on the host, so it uses `std`.

pub mod control;
pub mod harness;
pub mod i2c;
pub mod packrat;
pub mod supervisor;
pub mod sys;
