// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Helpers for orchestrator tasks.
//! robo:
//! robo: A scenario fails by calling [`fail`], which prints a message and
//! robo: halts the orchestrator; the host kernel then stops with a non-zero
//! robo: exit code. It passes through [`Supervisor::pass`].
//! robo:
//! robo: [`Supervisor::pass`]: crate::supervisor::Supervisor::pass

use core::fmt::Display;

/// robo: Environment variable naming the scenario to run. `xtask host-test`
/// robo: sets it; host tasks inherit the kernel's environment.
pub const SCENARIO_ENV: &str = "HUBRIS_HOST_TEST_SCENARIO";

/// robo: Interval at which [`wait_until`] re-checks its condition, in ms.
pub const POLL_MS: u64 = 10;

/// robo: The scenario requested by `xtask host-test`.
pub fn scenario() -> String {
    std::env::var(SCENARIO_ENV)
        .unwrap_or_else(|_| fail(format!("{SCENARIO_ENV} is not set")))
}

/// robo: Current kernel time, in ms.
pub fn now() -> u64 {
    userlib::sys_get_timer().now
}

pub fn sleep(ms: u64) {
    userlib::hl::sleep_for(ms);
}

/// robo: Fails the scenario.
pub fn fail(msg: impl Display) -> ! {
    eprintln!("host-test: FAIL at t={}: {msg}", now());
    userlib::hl::halt()
}

/// robo: Fails the scenario with a formatted message if `cond` is false.
#[macro_export]
macro_rules! check {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            $crate::harness::fail(format!($($arg)+));
        }
    };
}

/// robo: Prints a progress message.
pub fn log(msg: impl Display) {
    eprintln!("host-test: t={}: {msg}", now());
}

/// robo: Polls `f` every [`POLL_MS`] until it returns `Some`, and returns
/// robo: that value. Fails if `timeout_ms` passes first.
pub fn wait_until<T>(
    timeout_ms: u64,
    what: impl Display,
    mut f: impl FnMut() -> Option<T>,
) -> T {
    let deadline = now() + timeout_ms;
    loop {
        if let Some(v) = f() {
            return v;
        }
        if now() >= deadline {
            fail(format!(
                "timed out after {timeout_ms} ms waiting for {what}"
            ));
        }
        sleep(POLL_MS);
    }
}
