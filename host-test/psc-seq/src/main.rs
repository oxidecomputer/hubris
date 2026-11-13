// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Orchestrator for the psc-seq-server host test image.
//! robo:
//! robo: Runs the scenario named by `HUBRIS_HOST_TEST_SCENARIO`. Each
//! robo: scenario sets up PSUs, drives their pins and PMBus registers through
//! robo: the fakes, and checks what psc-seq-server did: `ENABLE_L` pin
//! robo: modes and levels (fake-sys log) and ereports (fake-packrat).
//! robo:
//! robo: A PSU's behavior (e.g. `PWR_OK` dropping while it is held off) is
//! robo: modelled here, step by step, rather than in the fakes.

use drv_stm32xx_sys_api::Port;
use host_test_api::check;
use host_test_api::harness::{fail, log, now, scenario, sleep, wait_until};
use host_test_api::i2c::{Addr, FakeI2c, addr_of, mwocp68};
use host_test_api::packrat::{Ereport, FakePackrat};
use host_test_api::supervisor::Supervisor;
use host_test_api::sys::{EventKind, FakeSys, Mode, Pin};
use userlib::task_slot;

task_slot!(SUPERVISOR, supervisor);
task_slot!(SYS, sys);
task_slot!(I2C, i2c_driver);
task_slot!(PACKRAT, packrat);
task_slot!(SEQUENCER, sequencer);

const PSU_COUNT: usize = 6;

// robo: Timing constants, copied from drv/psc-seq-server/src/main.rs.
const INSERT_DEBOUNCE_MS: u64 = 1_000;
const FAULT_OFF_MS: u64 = 5_000;
const PROBATION_MS: u64 = 4_000;
const POLL_MS: u64 = 500;

// robo: Pin assignments, copied from drv/psc-seq-server/src/bsp/psc_bc.rs.
const STATUS_LED: Pin = Pin {
    port: Port::A as u8,
    pin: 3,
};

fn present_l(psu: usize) -> Pin {
    Pin {
        port: Port::J as u8,
        pin: psu as u8,
    }
}

fn pwr_ok(psu: usize) -> Pin {
    Pin {
        port: Port::J as u8,
        pin: 6 + psu as u8,
    }
}

fn enable_l(psu: usize) -> Pin {
    Pin {
        port: Port::K as u8,
        pin: psu as u8,
    }
}

const ENABLE_L_PORT: u8 = Port::K as u8;
const ALL_ENABLE_L: u16 = 0b11_1111;

const CLASS_BAD: &str = "hw.pwr.pwr_good.bad";
const CLASS_GOOD: &str = "hw.pwr.pwr_good.good";
const CLASS_INSERT: &str = "hw.insert.psu";
const CLASS_REMOVE: &str = "hw.remove.psu";

/// robo: Handles to the fakes, and PSC-specific helpers on top of them.
struct Psc {
    sys: FakeSys,
    i2c: FakeI2c,
    packrat: FakePackrat,
    supervisor: Supervisor,
    psu_addrs: [Addr; PSU_COUNT],
}

impl Psc {
    fn new() -> Self {
        let i2c = I2C.get_task_id();
        let devs = [
            i2c_config::pmbus::v54_psu0,
            i2c_config::pmbus::v54_psu1,
            i2c_config::pmbus::v54_psu2,
            i2c_config::pmbus::v54_psu3,
            i2c_config::pmbus::v54_psu4,
            i2c_config::pmbus::v54_psu5,
        ];
        Self {
            sys: FakeSys(SYS.get_task_id()),
            i2c: FakeI2c(i2c),
            packrat: FakePackrat(PACKRAT.get_task_id()),
            supervisor: Supervisor(SUPERVISOR.get_task_id()),
            psu_addrs: devs.map(|f| addr_of(&f(i2c).0)),
        }
    }

    fn serial(psu: usize) -> String {
        format!("SN-TEST-PSU{psu}")
    }

    /// robo: Plugs PSU `psu` in: it answers on PMBus and asserts presence.
    /// robo: `PWR_OK` is left as it was (the board pulls it high).
    fn insert(&self, psu: usize) {
        self.i2c
            .attach(self.psu_addrs[psu], mwocp68::healthy(&Self::serial(psu)));
        self.sys.set_external(present_l(psu), false);
    }

    /// robo: Unplugs PSU `psu`. `PWR_OK` floats high, as on the board.
    fn remove(&self, psu: usize) {
        self.sys.set_external(present_l(psu), true);
        self.sys.set_external(pwr_ok(psu), true);
        self.i2c.detach(self.psu_addrs[psu]);
    }

    fn set_power_good(&self, psu: usize, good: bool) {
        self.sys.set_external(pwr_ok(psu), good);
    }

    /// robo: Sets up the board with the PSUs in `present` plugged in and
    /// robo: healthy, then waits for psc-seq-server to finish starting up
    /// robo: (it turns the status LED on).
    /// robo:
    /// robo: This runs before psc-seq-server reads any pin: it sleeps for
    /// robo: its settle time first, and virtual time doesn't advance while
    /// robo: any task can run.
    fn boot(&self, present: [bool; PSU_COUNT]) -> u64 {
        for psu in 0..PSU_COUNT {
            self.sys.set_external(pwr_ok(psu), true);
            self.sys.set_external(enable_l(psu), false);
            self.sys.set_external(present_l(psu), true);
            if present[psu] {
                self.insert(psu);
            }
        }
        self.wait_for_led_on(0)
    }

    /// robo: Waits until psc-seq-server turns the status LED on at or after
    /// robo: `since`, and returns when it did.
    fn wait_for_led_on(&self, since: u64) -> u64 {
        let led = 1 << STATUS_LED.pin;
        wait_until(30_000, "psc-seq-server to start", || {
            self.sys.events().iter().find_map(|e| match e.kind {
                EventKind::SetReset { port, set, .. }
                    if e.time >= since
                        && port == STATUS_LED.port
                        && set & led != 0 =>
                {
                    Some(e.time)
                }
                _ => None,
            })
        })
    }

    /// robo: Whether psc-seq-server is holding PSU `psu` off.
    fn held_off(&self, psu: usize) -> bool {
        let p = self.sys.pin(enable_l(psu));
        p.driving() && p.level()
    }

    /// robo: Waits until `held_off(psu) == off`, and returns the time of the
    /// robo: `ENABLE_L` mode change that made it so (from the fake-sys log,
    /// robo: so it isn't rounded up to the polling interval).
    fn wait_held_off(&self, psu: usize, off: bool, timeout: u64) -> u64 {
        let what = format!(
            "PSU {psu} to be {}",
            if off { "held off" } else { "released" }
        );
        wait_until(timeout, what, || (self.held_off(psu) == off).then_some(()));
        self.sys
            .events()
            .iter()
            .rev()
            .find_map(|e| match e.kind {
                EventKind::Configure {
                    port, pins, mode, ..
                } if port == ENABLE_L_PORT
                    && pins & (1 << psu) != 0
                    && (mode == Mode::Output) == off =>
                {
                    Some(e.time)
                }
                _ => None,
            })
            .unwrap_or_else(|| fail(format!("no ENABLE_L {psu} change logged")))
    }

    /// robo: Waits for the next ereport of `class` at index `from` or later
    /// robo: in the delivery order.
    fn wait_ereport(&self, from: usize, class: &str, timeout: u64) -> Ereport {
        wait_until(timeout, format!("a {class} ereport"), || {
            self.packrat
                .ereports_from(from)
                .into_iter()
                .find(|e| e.class() == class)
        })
    }

    /// robo: Classes of every ereport so far, in order.
    fn ereport_classes(&self) -> Vec<String> {
        self.packrat.ereports().iter().map(|e| e.class()).collect()
    }

    /// robo: Times `ENABLE_L` pins in `mask` were switched to output.
    fn enable_l_driven_at(&self, mask: u16) -> Vec<u64> {
        self.sys
            .events()
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::Configure {
                    port,
                    pins,
                    mode: Mode::Output,
                    ..
                } if port == ENABLE_L_PORT && pins & mask != 0 => Some(e.time),
                _ => None,
            })
            .collect()
    }

    /// robo: Checks that no `ENABLE_L` pin other than `psu`'s (if any) was
    /// robo: ever driven.
    fn check_others_never_driven(&self, psu: Option<usize>) {
        let others = psu.map_or(ALL_ENABLE_L, |p| ALL_ENABLE_L & !(1 << p));
        let driven = self.enable_l_driven_at(others);
        check!(
            driven.is_empty(),
            "other ENABLE_L pins were driven at {driven:?}"
        );
    }
}

fn check_slot(e: &Ereport, psu: usize) {
    let slot = e.field("slot");
    check!(
        slot == serde_json::json!(psu),
        "{} ereport is for slot {slot}, expected {psu}",
        e.class()
    );
}

fn check_serial(e: &Ereport, psu: usize) {
    let serial = e.field("fruid").get("serial").cloned();
    let want = serde_json::json!(Psc::serial(psu));
    check!(
        serial.as_ref() == Some(&want),
        "{} ereport has serial {serial:?}, expected {want}",
        e.class()
    );
}

// robo: ==================================================================
// robo: Scenarios
// robo: ==================================================================
// robo:
// robo: Each function below is one scenario: one story about the task under
// robo: test, run on a freshly booted image. This section is written to be
// robo: read as a guide to writing scenarios for other tasks.
// robo:
// robo: Shape of a scenario
// robo: -------------------
// robo: 1. Arrange: boot into a known state, then let the task settle.
// robo: 2. Act: change one input through a fake (a pin level, a device
// robo:    appearing or disappearing, a task restart).
// robo: 3. Observe: wait for the first externally visible reaction.
// robo: 4. Model: play the part of the hardware, e.g. a device recovering
// robo:    some time after being re-enabled, by changing inputs again.
// robo: 5. Assert the whole history: after the story ends, check everything
// robo:    the task did, including the things it must NOT have done.
// robo:
// robo: Observe at the task's boundaries
// robo: --------------------------------
// robo: Check only what the task does to the outside world: pin modes and
// robo: levels (fake-sys), bus transfers (fake-i2c), ereports
// robo: (fake-packrat), IPC replies. Don't reach into ringbufs or internal
// robo: state. Boundary checks survive refactors of the task; checks on
// robo: internals fail on harmless changes and pass on real bugs.
// robo:
// robo: Time
// robo: ----
// robo: - Time is virtual and the run is deterministic. A long `sleep` costs
// robo:   no wall time, and a flaky scenario points to a real bug in the task
// robo:   or the harness, never to scheduling noise.
// robo: - Take timestamps from the fakes' logs (`Event::time`,
// robo:   `Ereport::time`, `wait_held_off`'s return value), not from `now()`
// robo:   when a poll noticed something. `wait_until` polls every
// robo:   `POLL_MS`, so a time it observes can be up to 10 ms late. The first
// robo:   version of these scenarios measured from poll time and failed by a
// robo:   few ms against correct code.
// robo: - A `wait_*` timeout is an upper bound: "this must happen within X".
// robo:   Keep it tight for immediate reactions (100 ms for an
// robo:   interrupt-driven response) and set it to "constant + margin" for
// robo:   timer-driven ones. Lower bounds ("not before X") never come from a
// robo:   timeout; check them explicitly with `check!` on logged times.
// robo: - Timing constants are copied from the task (see the top of this
// robo:   file). If the task's constants change, these must too.
// robo:
// robo: Make failures easy to read
// robo: --------------------------
// robo: - `log()` each step of the story. On failure the output reads as a
// robo:   timeline that ends at the failing check.
// robo: - Put measured and expected values in every `check!` message.
// robo:
// robo: Choosing inputs
// robo: ---------------
// robo: - Use a different, non-zero unit index in each scenario (here, PSUs
// robo:   1-4). Mapping bugs between indices, pins, notification bits and
// robo:   I2C addresses hide when every test uses index 0 or the same index.
// robo: - Act on one unit and check that every other unit was left alone
// robo:   (`check_others_never_driven`).
// robo:
// robo: Prove the scenario can fail
// robo: ---------------------------
// robo: Before trusting a new scenario, break the behavior it covers in the
// robo: task (shorten a timeout, skip a step) and run it. If it still
// robo: passes, it isn't checking what you think. Doing this for these
// robo: scenarios showed that `fault_persists` didn't check the off period
// robo: until a per-cycle check was added.
// robo:
// robo: Adding a scenario
// robo: -----------------
// robo: Write the function, add its name to `scenarios` in this image's
// robo: `app.toml`, and add it to the `match` in `main`.

/// robo: All PSUs present and healthy at boot. psc-seq-server must never
/// robo: drive an `ENABLE_L` pin, and must report nothing.
// robo:
// robo: The "nothing happens" scenario. Write one for every task: it checks
// robo: the default path, and it is the baseline that makes the other
// robo: scenarios' "only this changed" checks meaningful. Here it also
// robo: covers the rack's most important rule: a starting PSC must not turn
// robo: off a running PSU.
fn boot_all_present(psc: &Psc) {
    // robo: Arrange. `boot` sets every input before the task reads any, and
    // robo: returns once the task signals that startup finished (status LED
    // robo: on). Prefer an observable readiness signal like this to a fixed
    // robo: sleep from t=0, so the scenario doesn't depend on how long
    // robo: startup takes.
    psc.boot([true; PSU_COUNT]);
    // robo: Let every startup timer run out (probation, plus several poll
    // robo: periods). "Nothing happened" is only worth asserting after the
    // robo: point where something could have happened.
    sleep(PROBATION_MS + 5 * POLL_MS);

    // robo: Negative check over the whole run: scan the full fake-sys log
    // robo: rather than sampling current pin state. A pin that was driven
    // robo: and later released looks fine now, but it already glitched the
    // robo: PSU.
    psc.check_others_never_driven(None);
    // robo: Positive check on a precondition the task relies on: output
    // robo: levels are preloaded high, so a later switch to output mode
    // robo: drives high immediately rather than briefly driving low.
    // robo: Checking it here means a regression shows up as "never
    // robo: preloaded", not as a puzzling failure in a fault scenario.
    let preloaded = psc.sys.events().iter().any(|e| {
        matches!(e.kind, EventKind::SetReset { port, set, .. }
            if port == ENABLE_L_PORT && set & ALL_ENABLE_L == ALL_ENABLE_L)
    });
    check!(
        preloaded,
        "ENABLE_L output levels were never preloaded high"
    );
    // robo: End state: every enable pin is an undriven input. Checked per
    // robo: unit so the message names the one that's wrong.
    for psu in 0..PSU_COUNT {
        let p = psc.sys.pin(enable_l(psu));
        check!(
            p.mode == Mode::Input,
            "ENABLE_L {psu} is in mode {:?}",
            p.mode
        );
    }
    // robo: Exact ereport history: here, none. Every scenario ends with this
    // robo: check; extra or duplicate reports are bugs too.
    let classes = psc.ereport_classes();
    check!(classes.is_empty(), "unexpected ereports: {classes:?}");
}

/// robo: A PSU faults, is held off for `FAULT_OFF_MS`, is released, and
/// robo: comes back.
// robo:
// robo: The main fault-handling path, start to finish. Its structure is the
// robo: template for most scenarios: act, check the immediate reaction,
// robo: check each timed phase's lower bound from logged times, model the
// robo: hardware's response, then check the whole history.
fn fault_recover(psc: &Psc) {
    let psu = 2;
    // robo: Arrange, then wait out boot-time probation so the fault lands in
    // robo: steady state. A fault during probation is a different story and
    // robo: would get its own scenario.
    psc.boot([true; PSU_COUNT]);
    sleep(PROBATION_MS + POLL_MS);

    // robo: Act: one input change. `PWR_OK` is interrupt-driven, so the
    // robo: reaction should be immediate; the 100 ms timeouts below say so.
    log(format!("PSU {psu} PWR_OK drops"));
    psc.set_power_good(psu, false);
    // robo: Observe the report first, then its contents. Checking the slot
    // robo: and the serial number (served by fake-i2c) checks that the
    // robo: ereport was assembled from the right unit's data, not just that
    // robo: some ereport of the right class went out.
    let bad = psc.wait_ereport(0, CLASS_BAD, 100);
    check_slot(&bad, psu);
    check_serial(&bad, psu);
    // robo: `t_off` is the logged time of the mode change, so the interval
    // robo: below is exact.
    let t_off = psc.wait_held_off(psu, true, 100);

    // robo: Timed phase. The timeout (constant + 1 s) is the upper bound;
    // robo: the explicit check is the lower bound, measured between two
    // robo: logged events. Splitting them this way gives a clear message
    // robo: in each direction: "timed out waiting" versus "released after
    // robo: N ms, expected at least M".
    let t_on = psc.wait_held_off(psu, false, FAULT_OFF_MS + 1_000);
    check!(
        t_on - t_off >= FAULT_OFF_MS,
        "released after {} ms, expected at least {FAULT_OFF_MS}",
        t_on - t_off
    );

    // robo: Model the hardware: a PSU takes a while to come up after being
    // robo: released. The scenario owns this behavior, not fake-sys. Pick a
    // robo: delay inside the task's tolerance (here, under `PROBATION_MS`)
    // robo: for a recovery story, or outside it for a failure story.
    sleep(1_000);
    log(format!("PSU {psu} comes back"));
    psc.set_power_good(psu, true);
    // robo: `from = 1` skips the ereport already seen, so this can only
    // robo: match a new one. Always pass the index of the next ereport you
    // robo: expect.
    let good = psc.wait_ereport(1, CLASS_GOOD, PROBATION_MS + 1_000);
    check_slot(&good, psu);
    // robo: Lower bound again: recovery must not be declared before
    // robo: probation ends, even though `PWR_OK` came back sooner.
    check!(
        good.time - t_on >= PROBATION_MS,
        "recovery reported {} ms after release, before probation ended",
        good.time - t_on
    );

    // robo: Whole-history checks: no other unit touched, and exactly the
    // robo: expected ereports in order.
    psc.check_others_never_driven(Some(psu));
    let classes = psc.ereport_classes();
    check!(
        classes == [CLASS_BAD, CLASS_GOOD],
        "unexpected ereports: {classes:?}"
    );
}

/// robo: A PSU faults and stays bad. psc-seq-server must keep cycling it
/// robo: without reporting a new fault each cycle, then report recovery
/// robo: once it comes back.
// robo:
// robo: Repeated behavior. Run a bounded number of cycles (three here: the
// robo: first, a repeat, and one more to show the repeat is stable) and
// robo: check every cycle's timing, not just the first. A bug that appears
// robo: only on the second pass, such as a timer not being re-armed, is
// robo: what this kind of scenario is for.
fn fault_persists(psc: &Psc) {
    let psu = 3;
    psc.boot([true; PSU_COUNT]);
    sleep(PROBATION_MS + POLL_MS);

    // robo: Act once. The model for this story is "the PSU never recovers":
    // robo: `PWR_OK` stays low through every cycle below.
    psc.set_power_good(psu, false);
    psc.wait_ereport(0, CLASS_BAD, 100);
    let mut t_off = psc.wait_held_off(psu, true, 100);
    let mut cycle = 1;
    let t_on = loop {
        // robo: Off period of this cycle. This check was missing at first;
        // robo: shortening `FAULT_OFF_MS` in the task didn't fail this
        // robo: scenario until it was added. Every timed phase needs its own
        // robo: lower-bound check.
        let t_on = psc.wait_held_off(psu, false, FAULT_OFF_MS + 1_000);
        check!(
            t_on - t_off >= FAULT_OFF_MS,
            "cycle {cycle}: released after {} ms, expected at least \
             {FAULT_OFF_MS}",
            t_on - t_off
        );
        if cycle == 3 {
            break t_on;
        }
        // robo: Probation of this cycle: with `PWR_OK` still low, the task
        // robo: must give up and disable the PSU again, but not early.
        log(format!("cycle {cycle}: released, PSU stays bad"));
        t_off = psc.wait_held_off(psu, true, PROBATION_MS + 1_000);
        check!(
            t_off - t_on >= PROBATION_MS,
            "re-disabled {} ms after release, before probation ended",
            t_off - t_on
        );
        cycle += 1;
    };

    // robo: End the story with a recovery, so the scenario also checks that
    // robo: cycling doesn't leave the task in a state it can't leave.
    log("PSU comes back");
    psc.set_power_good(psu, true);
    let good = psc.wait_ereport(0, CLASS_GOOD, PROBATION_MS + 1_000);
    check!(
        good.time - t_on >= PROBATION_MS,
        "recovered before probation"
    );

    // robo: The exact-history check is what proves "no new fault report per
    // robo: cycle": three cycles, still only one `bad`.
    psc.check_others_never_driven(Some(psu));
    let classes = psc.ereport_classes();
    check!(
        classes == [CLASS_BAD, CLASS_GOOD],
        "unexpected ereports: {classes:?}"
    );
}

/// robo: A PSU absent at boot is inserted, removed, and reinserted. Removal
/// robo: holds it off; reinsertion releases it only after the debounce.
// robo:
// robo: Hot-plug. Cover a unit that is absent at boot (its own startup
// robo: path), its first insertion, removal, and a second insertion; the
// robo: second can differ from the first because removal changed state
// robo: (here, the enable pin is held).
fn remove_insert(psc: &Psc) {
    let psu = 4;
    let mut present = [true; PSU_COUNT];
    present[psu] = false;
    psc.boot(present);
    sleep(PROBATION_MS);
    // robo: Check the absent unit's boot state before acting, so a failure
    // robo: later can't be blamed on the wrong step.
    check!(!psc.held_off(psu), "absent PSU {psu} held off at boot");

    // robo: Insertion is a composite input: the device answers on I2C and
    // robo: the presence pin asserts. `insert` does both in one helper, in
    // robo: the order the hardware would (device first), so every scenario
    // robo: inserts the same way.
    log(format!("insert PSU {psu}"));
    let t_ins = now();
    psc.insert(psu);
    // robo: Presence is polled, not interrupt-driven, so the timeout
    // robo: allows the debounce plus two poll periods: one to notice the
    // robo: insertion, one for the fresh sample taken after the debounce.
    let ins =
        psc.wait_ereport(0, CLASS_INSERT, INSERT_DEBOUNCE_MS + 2 * POLL_MS);
    check_slot(&ins, psu);
    check_serial(&ins, psu);
    // robo: Weak lower bound: `t_ins` is when the input changed, but the
    // robo: debounce starts when the task notices, which isn't observable.
    // robo: Note checks like this where they exist; the reinsertion check
    // robo: below measures the same debounce more directly through the
    // robo: enable pin.
    check!(
        ins.time - t_ins >= INSERT_DEBOUNCE_MS,
        "insertion reported after {} ms, before the debounce ended",
        ins.time - t_ins
    );
    sleep(PROBATION_MS);

    // robo: Removal. The removal ereport is sent after the device is gone,
    // robo: so its serial number must come from what the task cached at
    // robo: insertion. Checking the serial here tests that cache.
    log(format!("remove PSU {psu}"));
    psc.remove(psu);
    let rem = psc.wait_ereport(1, CLASS_REMOVE, 2 * POLL_MS);
    check_slot(&rem, psu);
    check_serial(&rem, psu);
    psc.wait_held_off(psu, true, 100);
    sleep(2_000);

    // robo: Reinsertion while held off: the pin must stay driven through
    // robo: the debounce. Measuring from the input change to the logged
    // robo: release gives a firm lower bound.
    log(format!("reinsert PSU {psu}"));
    let t_re = now();
    psc.insert(psu);
    let t_on = psc.wait_held_off(psu, false, INSERT_DEBOUNCE_MS + 2 * POLL_MS);
    check!(
        t_on - t_re >= INSERT_DEBOUNCE_MS,
        "released {} ms after reinsertion, before the debounce ended",
        t_on - t_re
    );
    psc.wait_ereport(2, CLASS_INSERT, 100);

    psc.check_others_never_driven(Some(psu));
    let classes = psc.ereport_classes();
    check!(
        classes == [CLASS_INSERT, CLASS_REMOVE, CLASS_INSERT],
        "unexpected ereports: {classes:?}"
    );
}

/// robo: psc-seq-server restarts while holding a faulted PSU off. The new
/// robo: incarnation must keep it off, resume the off period without a new
/// robo: fault report, and then release it.
// robo:
// robo: Restart. Every task that drives hardware should have one: the
// robo: hardware keeps its state across a task restart, and the new
// robo: incarnation has to work out where the old one left off. The fakes
// robo: are separate tasks, so their state survives the restart just like
// robo: real hardware state would. Restart in the middle of an interesting
// robo: phase, not in steady state.
fn restart_while_disabled(psc: &Psc) {
    let psu = 1;
    psc.boot([true; PSU_COUNT]);
    sleep(PROBATION_MS + POLL_MS);

    // robo: Get into the phase to interrupt (held off after a fault), then
    // robo: go partway into it, so "resumed" and "started over" give
    // robo: different timings.
    psc.set_power_good(psu, false);
    psc.wait_ereport(0, CLASS_BAD, 100);
    psc.wait_held_off(psu, true, 100);
    sleep(1_000);

    // robo: Act: the restart itself, through the supervisor. Then wait for
    // robo: the readiness signal *after* the restart time; the LED-on event
    // robo: from the first boot is still in the log and must not match.
    log("restart psc-seq-server");
    let t_restart = now();
    psc.supervisor.restart_task(SEQUENCER.get_task_index());
    let t_ready = psc.wait_for_led_on(t_restart);

    // robo: The new incarnation runs a full off period from its own start.
    // robo: Measuring from `t_ready` (not the original fault) checks that
    // robo: it neither released immediately nor counted time it didn't see.
    let t_on = psc.wait_held_off(psu, false, FAULT_OFF_MS + 1_000);
    check!(
        t_on - t_ready >= FAULT_OFF_MS,
        "released {} ms after restart, expected at least {FAULT_OFF_MS}",
        t_on - t_ready
    );
    // robo: Negative check over a window, from the log: no release of this
    // robo: pin between the restart and the intended release. The pin's
    // robo: state at either end would look right even if it had been
    // robo: released and re-driven in between; only the log shows that.
    let released_early = psc.sys.events().iter().any(|e| {
        e.time >= t_restart
            && e.time < t_on
            && matches!(e.kind, EventKind::Configure { port, pins, mode, .. }
                if port == ENABLE_L_PORT
                    && pins & (1 << psu) != 0
                    && mode != Mode::Output)
    });
    check!(!released_early, "PSU {psu} was released during the restart");

    psc.set_power_good(psu, true);
    psc.wait_ereport(1, CLASS_GOOD, PROBATION_MS + 1_000);

    // robo: The exact-history check proves the restart didn't send a second
    // robo: `bad` report.
    psc.check_others_never_driven(Some(psu));
    let classes = psc.ereport_classes();
    check!(
        classes == [CLASS_BAD, CLASS_GOOD],
        "unexpected ereports: {classes:?}"
    );
}

fn main() -> ! {
    let psc = Psc::new();
    let name = scenario();
    log(format!("scenario {name}"));
    match name.as_str() {
        "boot_all_present" => boot_all_present(&psc),
        "fault_recover" => fault_recover(&psc),
        "fault_persists" => fault_persists(&psc),
        "remove_insert" => remove_insert(&psc),
        "restart_while_disabled" => restart_while_disabled(&psc),
        _ => fail(format!("unknown scenario {name}")),
    }
    psc.supervisor.pass()
}

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));
