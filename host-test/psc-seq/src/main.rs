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

/// robo: All PSUs present and healthy at boot. psc-seq-server must never
/// robo: drive an `ENABLE_L` pin, and must report nothing.
fn boot_all_present(psc: &Psc) {
    psc.boot([true; PSU_COUNT]);
    sleep(PROBATION_MS + 5 * POLL_MS);

    psc.check_others_never_driven(None);
    let preloaded = psc.sys.events().iter().any(|e| {
        matches!(e.kind, EventKind::SetReset { port, set, .. }
            if port == ENABLE_L_PORT && set & ALL_ENABLE_L == ALL_ENABLE_L)
    });
    check!(
        preloaded,
        "ENABLE_L output levels were never preloaded high"
    );
    for psu in 0..PSU_COUNT {
        let p = psc.sys.pin(enable_l(psu));
        check!(
            p.mode == Mode::Input,
            "ENABLE_L {psu} is in mode {:?}",
            p.mode
        );
    }
    let classes = psc.ereport_classes();
    check!(classes.is_empty(), "unexpected ereports: {classes:?}");
}

/// robo: A PSU faults, is held off for `FAULT_OFF_MS`, is released, and
/// robo: comes back.
fn fault_recover(psc: &Psc) {
    let psu = 2;
    psc.boot([true; PSU_COUNT]);
    sleep(PROBATION_MS + POLL_MS);

    log(format!("PSU {psu} PWR_OK drops"));
    psc.set_power_good(psu, false);
    let bad = psc.wait_ereport(0, CLASS_BAD, 100);
    check_slot(&bad, psu);
    check_serial(&bad, psu);
    let t_off = psc.wait_held_off(psu, true, 100);

    let t_on = psc.wait_held_off(psu, false, FAULT_OFF_MS + 1_000);
    check!(
        t_on - t_off >= FAULT_OFF_MS,
        "released after {} ms, expected at least {FAULT_OFF_MS}",
        t_on - t_off
    );

    sleep(1_000);
    log(format!("PSU {psu} comes back"));
    psc.set_power_good(psu, true);
    let good = psc.wait_ereport(1, CLASS_GOOD, PROBATION_MS + 1_000);
    check_slot(&good, psu);
    check!(
        good.time - t_on >= PROBATION_MS,
        "recovery reported {} ms after release, before probation ended",
        good.time - t_on
    );

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
fn fault_persists(psc: &Psc) {
    let psu = 3;
    psc.boot([true; PSU_COUNT]);
    sleep(PROBATION_MS + POLL_MS);

    psc.set_power_good(psu, false);
    psc.wait_ereport(0, CLASS_BAD, 100);
    let mut t_off = psc.wait_held_off(psu, true, 100);
    let mut cycle = 1;
    let t_on = loop {
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
        log(format!("cycle {cycle}: released, PSU stays bad"));
        t_off = psc.wait_held_off(psu, true, PROBATION_MS + 1_000);
        check!(
            t_off - t_on >= PROBATION_MS,
            "re-disabled {} ms after release, before probation ended",
            t_off - t_on
        );
        cycle += 1;
    };

    log("PSU comes back");
    psc.set_power_good(psu, true);
    let good = psc.wait_ereport(0, CLASS_GOOD, PROBATION_MS + 1_000);
    check!(
        good.time - t_on >= PROBATION_MS,
        "recovered before probation"
    );

    psc.check_others_never_driven(Some(psu));
    let classes = psc.ereport_classes();
    check!(
        classes == [CLASS_BAD, CLASS_GOOD],
        "unexpected ereports: {classes:?}"
    );
}

/// robo: A PSU absent at boot is inserted, removed, and reinserted. Removal
/// robo: holds it off; reinsertion releases it only after the debounce.
fn remove_insert(psc: &Psc) {
    let psu = 4;
    let mut present = [true; PSU_COUNT];
    present[psu] = false;
    psc.boot(present);
    sleep(PROBATION_MS);
    check!(!psc.held_off(psu), "absent PSU {psu} held off at boot");

    log(format!("insert PSU {psu}"));
    let t_ins = now();
    psc.insert(psu);
    let ins =
        psc.wait_ereport(0, CLASS_INSERT, INSERT_DEBOUNCE_MS + 2 * POLL_MS);
    check_slot(&ins, psu);
    check_serial(&ins, psu);
    check!(
        ins.time - t_ins >= INSERT_DEBOUNCE_MS,
        "insertion reported after {} ms, before the debounce ended",
        ins.time - t_ins
    );
    sleep(PROBATION_MS);

    log(format!("remove PSU {psu}"));
    psc.remove(psu);
    let rem = psc.wait_ereport(1, CLASS_REMOVE, 2 * POLL_MS);
    check_slot(&rem, psu);
    check_serial(&rem, psu);
    psc.wait_held_off(psu, true, 100);
    sleep(2_000);

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
fn restart_while_disabled(psc: &Psc) {
    let psu = 1;
    psc.boot([true; PSU_COUNT]);
    sleep(PROBATION_MS + POLL_MS);

    psc.set_power_good(psu, false);
    psc.wait_ereport(0, CLASS_BAD, 100);
    psc.wait_held_off(psu, true, 100);
    sleep(1_000);

    log("restart psc-seq-server");
    let t_restart = now();
    psc.supervisor.restart_task(SEQUENCER.get_task_index());
    let t_ready = psc.wait_for_led_on(t_restart);

    let t_on = psc.wait_held_off(psu, false, FAULT_OFF_MS + 1_000);
    check!(
        t_on - t_ready >= FAULT_OFF_MS,
        "released {} ms after restart, expected at least {FAULT_OFF_MS}",
        t_on - t_ready
    );
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
