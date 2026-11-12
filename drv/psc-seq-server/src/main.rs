// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the PSC sequencing process.
//!
//! # Structure
//!
//! This task runs the [`hubris_async`] executor with one async future per PSU
//! (see [`psu_lifecycle`]), plus one [`pin_sampler`] future that reads every
//! PSU's presence and `PWR_OK` pins, on any `PWR_OK` edge and every `POLL_MS`.
//! It publishes each sample to [`PINS`], a [`Watch`], and the PSU futures
//! learn about their pins only from there; they never touch GPIOs or IRQs.
//!
//! Each PSU's sequencing state is expressed as plain async control flow
//! rather than an explicit state-machine enum: the "wait" states (debounce,
//! probation, fault-off) are `Timer::after().await`, and *removal* is a single
//! structural boundary — everything that happens while a PSU is present runs
//! inside `select(wait_for_presence(Present::No), monitor_present_psu)`, so a
//! removal at any point cancels the present-side future and is handled in one
//! place. Because the removal watch is polled first, on the same sample, the
//! present side only ever runs on a sample showing the PSU present (see
//! [`monitor_until_removed`]).
//!
//! Cancellation is safe because every `.await` in the PSU futures is a timer
//! or a wait on [`PINS`], neither of which has side effects. PMBus/GPIO calls
//! are synchronous and never span an await, so a future dropped on removal
//! can't tear an I2C transaction or leave a pin half-driven.
//!
//! # Hardware Support
//!
//! The server supports both the original PSC's MWOCP68 PSU and the Observer's
//! MWOCP67 PSU, which have some annoying differences:
//!
//! 1) The MWOCP68 can be disabled via the `ON_L`/`ENABLE_L`/`PSKILL` signal,
//!    but the MWOCP67 can only be disabled via PMBus commands. This doesn't
//!    affect the fault recovery sequence much, but does affect hot-insertion.
//!    We use this signal to hold the MWOP68 off when it's first inserted, until
//!    the connector has had time to debounce. It's impossible to do the same
//!    for the MWOCP67 - it's enabled by default and will immediately start
//!    outputting power when it's inserted.
//!
//! 2) On the PSC, the `PRESENT_L` signals need to be polled. On the Observer,
//!    we could use external interrupts but don't yet (see hubris#2565)
//!
//! # General notes on PSC power supply sequencing
//!
//! There are rules to follow here to avoid glitching the power supplies,
//! because glitching the power supplies here will glitch the entire rack,
//! making you very unpopular very quickly.
//!
//! **`ON_L` signals to the PSUs (MWOCP68 only):** we normally leave our pins
//! high-impedance on these nets, allowing external resistors to pull them low.
//! We only drive them to _disable_ the PSU by driving it high. To achieve this,
//! we leave the pin configured as an input, pre-load the output value as
//! "high," and toggle its mode register between input and output.
//!
//! **`PRESENT_L` signals from the PSUs:** pulled inactive-high by resistors on
//! the board and power shelf. When these go low, assume they will bounce,
//! because they're brought low by a physical connection between pins on our
//! connector.
//!
//! **`OK` signals from the PSUs:** pulled ACTIVE-high by resistors on the
//! board, in all caps because resistors pulling something active is somewhat
//! unusual. The PSU drives this open drain, so it will only go low if the PSU
//! pulls it low to indicate a problem. This implies that, if the PSU is not
//! detected as present, you cannot trust the `OK` signal.
//!
//!
//! # Intended behavior
//!
//! Let's ignore task restarts / crashes for the moment.
//!
//! The PSC is intended to be hot swappable. If the PSC gets plugged in, this
//! task will start anew (along with the rest of the firmware), with RAM cleared
//! and peripherals in reset state. This will also happen if the PSC is plugged
//! into a rack that is then plugged into power -- we can't usefully distinguish
//! these cases, nor do we need to.
//!
//! When the PSC is *removed* from the system, the pull resistors on the power
//! supply ON signals cause the power supplies to turn on. It's important that
//! we don't override this when the PSC is reinserted. So, at startup, the PSC
//! must leave the ON lines undriven, allowing them to float low. (The MWOCP67
//! has no ON lines, so it merely needs to not send PMBus commands that disable
//! the PSUs.)
//!
//! Because the PSC's connector is not designed for hot swap, we can't
//! necessarily trust our inputs at power-on. Without a firm "all connections
//! made" indication from the connector, the best we can do is assume that the
//! connector insertion cycle will finish within some time interval. We delay
//! for this time interval before looking at any inputs. During this time, the
//! power supplies will be on.
//!
//! At that point, we start a per-PSU future that continuously does the
//! following, for each power supply separately:
//!
//! - Watch for the presence line to be high, meaning the PSU is removed.
//! - Record that the PSU is missing.
//! - Start driving its ON signal high. (MWOCP68 only)
//! - Wait for the presence line to be low, meaning the PSU is reinserted.
//! - Release its ON signal so it may turn on normally. (MWOCP68 only)
//!
//! Simultaneously, while the PSU is not removed, we monitor the OK signal for
//! indication of internal faults. If we find a fault, we...
//!
//! - Record as much information as we can reasonably gather.
//! - Force the PSU off, using the ON signal or a PMBus command.
//! - Wait some time to allow things to discharge.
//! - Turn the PSU back on.
//! - Wait some time for it to wake.
//! - Start watching the fault signal again.
//!
//! Removing and reinserting a PSU in general clears the fault state _and_
//! resets the retry sequence.
//!
//!
//! # Generalizing to task restarts
//!
//! This task is not intended to restart under normal operation, but bugs
//! happen. At task startup, we try to determine whether any of the PSUs were in
//! the middle of a fault recovery sequence (above). If a PSU appears to have
//! been turned off by our previous incarnation, we resume fault recovery on
//! that PSU from the off period, without reporting a new fault. This is the
//! [`InitialState::FoundDisabled`] start.
//!
//! Task crashes will reset the retry sequence. This is unavoidable without
//! keeping state across incarnations, which we're trying to avoid to reduce the
//! likelihood of crashloops.
//!
//! Task crashes may also reactivate a PSU that the control plane had commanded
//! off. Currently this is unavoidable; we might want to record such overrides
//! in the FRAM to be safe.

#![no_std]
#![no_main]

use core::convert::Infallible;

use drv_i2c_devices::mwocp6x;

use drv_packrat_vpd_loader::{Packrat, read_vpd_and_load_packrat};
use drv_psc_seq_api::PowerState;
use drv_stm32xx_sys_api as sys_api;
use sys_api::{Edge, IrqControl, OutputType, Pull, Speed, Sys};
use task_jefe_api::Jefe;
use userlib::{UnwrapLite, sys_get_timer, task_slot};

use hubris_async::time::{Timer, now};
use hubris_async::watch::Watch;
use hubris_async::{Either, Executor, notification, select};

use fixedstr::{FixedStr, FixedString};
use ringbuf::{counted_ringbuf, ringbuf_entry};
use static_cell::StaticCell;

task_slot!(SYS, sys);
task_slot!(I2C, i2c_driver);
task_slot!(JEFE, jefe);
task_slot!(PACKRAT, packrat);

#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum Event {
    #[count(skip)]
    None,
    /// Emitted at task startup when we find that a power supply is probably
    /// already on. (Note that if the power supply is not present, we will still
    /// detect it as "on" due to the pull resistors.)
    FoundEnabled {
        now: u64,
        #[count(children)]
        psu: Slot,
        serial: Option<FixedString<12>>,
    },
    /// Emitted at task startup when we find that a power supply appears to have
    /// been disabled.
    FoundAlreadyDisabled {
        now: u64,
        #[count(children)]
        psu: Slot,
        serial: Option<FixedString<12>>,
    },
    /// Emitted when a previously not present PSU's presence pin is asserted.
    Inserted {
        now: u64,
        #[count(children)]
        psu: Slot,
        serial: Option<FixedString<12>>,
    },
    /// Emitted when a previously present PSU's presence pin is deasserted.
    Removed {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Emitted after successfully performing
    /// `ActionRequired::EnableOnInsertion`.
    EnableOnInsertionSucceeded {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Emitted after failing to perform `ActionRequired::EnableOnInsertion`.
    EnableOnInsertionFailed {
        now: u64,
        #[count(children)]
        psu: Slot,
        err: mwocp6x::Error,
    },
    /// Emitted after successfully performing `ActionRequired::DisableOnFault`.
    DisableOnFaultSucceeded {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Emitted after failing to perform `ActionRequired::DisableOnFault`.
    DisableOnFaultFailed {
        now: u64,
        #[count(children)]
        psu: Slot,
        err: mwocp6x::Error,
    },
    /// Emitted after successfully performing
    /// `ActionRequired::ReEnableAfterFault`.
    ReEnableAfterFaultSucceeded {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Emitted after failing to perform `ActionRequired::ReEnableAfterFault`.
    ReEnableAfterFaultFailed {
        now: u64,
        #[count(children)]
        psu: Slot,
        err: mwocp6x::Error,
    },
    /// Emitted after successfully performing
    /// `ActionRequired::DisableOnRemoval`.
    DisableOnRemovalSucceeded {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Emitted after failing to perform `ActionRequired::DisableOnRemoval`.
    DisableOnRemovalFailed {
        now: u64,
        #[count(children)]
        psu: Slot,
        err: mwocp6x::Error,
    },
}

impl Event {
    fn action_result(
        action: ActionRequired,
        err: Option<mwocp6x::Error>,
        now: u64,
        psu: Slot,
    ) -> Event {
        match (action, err) {
            (ActionRequired::EnableOnInsertion, None) => {
                Event::EnableOnInsertionSucceeded { now, psu }
            }
            (ActionRequired::EnableOnInsertion, Some(err)) => {
                Event::EnableOnInsertionFailed { now, psu, err }
            }
            (ActionRequired::DisableOnFault, None) => {
                Event::DisableOnFaultSucceeded { now, psu }
            }
            (ActionRequired::DisableOnFault, Some(err)) => {
                Event::DisableOnFaultFailed { now, psu, err }
            }
            (ActionRequired::ReEnableAfterFault, None) => {
                Event::ReEnableAfterFaultSucceeded { now, psu }
            }
            (ActionRequired::ReEnableAfterFault, Some(err)) => {
                Event::ReEnableAfterFaultFailed { now, psu, err }
            }
            (ActionRequired::DisableOnRemoval, None) => {
                Event::DisableOnRemovalSucceeded { now, psu }
            }
            (ActionRequired::DisableOnRemoval, Some(err)) => {
                Event::DisableOnRemovalFailed { now, psu, err }
            }
        }
    }
}

// Since entries in this ringbuffer contain timestamps, they will never be
// de-duplicated. Thus, disable it.
counted_ringbuf!(Event, 128, Event::None, no_dedup);

/// More verbose debugging data goes in its own ring buffer, so that we can
/// maintain a longer history of major PSU events while still recording more
/// detailed information about the PSU's status.
///
/// Each of these entries has a `now` value which can be correlated with the
/// timestamps in the main ringbuf.
///
/// An entry for each of the rectifier's PMBus status registers (e.g.
/// `STATUS_WORD`, `STATUS_VOUT`, `STATUS_IOUT`, and so on...) is recorded read
/// whenever the state machine starts or finishes a fault recovery process.
/// Since exactly one of each register entry is recorded for every
/// `FaultRecoveryStarted` and `FaultRecoveryFinished` entry, we don't really
/// need to spend extra bytes on counting them, so they are marked as
/// `count(skip)`.
#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    /// The state machine detected a new fault and started the recovery process.
    FaultRecoveryStarted {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// The PSU has recovered from the fault and is outputting power again.
    FaultRecoveryFinished {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Power is now good, according to the PWR_OK pin
    PowerGoodAsserted {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Power is now ungood, according to the PWR_OK pin
    PowerGoodDeasserted {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    PowerStillUngood {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    #[count(skip)]
    StatusWord {
        now: u64,
        psu: Slot,
        status_word: Result<u16, mwocp6x::Error>,
    },
    #[count(skip)]
    StatusIout {
        now: u64,
        psu: Slot,
        status_iout: Result<u8, mwocp6x::Error>,
    },
    #[count(skip)]
    StatusVout {
        now: u64,
        psu: Slot,
        status_vout: Result<u8, mwocp6x::Error>,
    },
    #[count(skip)]
    StatusInput {
        now: u64,
        psu: Slot,
        status_input: Result<u8, mwocp6x::Error>,
    },
    #[count(skip)]
    StatusCml {
        now: u64,
        psu: Slot,
        status_cml: Result<u8, mwocp6x::Error>,
    },
    #[count(skip)]
    StatusTemperature {
        now: u64,
        psu: Slot,
        status_temperature: Result<u8, mwocp6x::Error>,
    },
    #[count(skip)]
    StatusMfrSpecific {
        now: u64,
        psu: Slot,
        status_mfr_specific: Result<u8, mwocp6x::Error>,
    },
}

impl Trace {
    fn power_good_changed(status: PowerStatus, now: u64, psu: Slot) -> Trace {
        match status {
            PowerStatus::Good => Trace::PowerGoodAsserted { now, psu },
            PowerStatus::NotGood => Trace::PowerGoodDeasserted { now, psu },
        }
    }
}

// Since entries in this ringbuffer contain timestamps, they will never be
// de-duplicated. Thus, disable it.
counted_ringbuf!(__TRACE, Trace, 32, Trace::None, no_dedup);

/// I2c error logging is even more verbose than the `Trace` ring buffer because
/// there are so many failed and retried i2c reads when a PSU is hot-plugged. We
/// put them in their own ring buffer so they don't drown out all the other
/// `Trace` events.
#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum I2cTrace {
    #[count(skip)]
    None,
    I2cError {
        now: u64,
        #[count(children)]
        psu: Slot,
        err: mwocp6x::Error,
    },
}
// Since entries in this ringbuffer contain timestamps, they will never be
// de-duplicated. Thus, disable it.
counted_ringbuf!(__I2C_TRACE, I2cTrace, 32, I2cTrace::None, no_dedup);

/// PSU numbers represented as an enum. This is intended for use with
/// `counted_ringbuf!`, instead of representing PSU numbers as raw u8s, which
/// cannot derive `counters::Count` (and would have to generate a counter table
/// with 256 entries rather than just 6).
#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
#[repr(u8)]
enum Slot {
    Psu0 = 0,
    Psu1 = 1,
    Psu2 = 2,
    Psu3 = 3,
    Psu4 = 4,
    Psu5 = 5,
}

// The per-PSU signal definitions in the bsp modules all refer to this constant
// for the number of PSUs. It's not intended to be easily configurable, since
// that'd require hardware changes.
pub const PSU_COUNT: usize = 6;

// Board-specific behavior is isolated into a `bsp` module, which is picked
// based on the target_board name.
#[cfg_attr(
    any(target_board = "psc-b", target_board = "psc-c"),
    path = "bsp/psc_bc.rs"
)]
#[cfg_attr(target_board = "observer-a", path = "bsp/observer_a.rs")]
mod bsp;

const PSU_SLOTS: [Slot; PSU_COUNT] = [
    Slot::Psu0,
    Slot::Psu1,
    Slot::Psu2,
    Slot::Psu3,
    Slot::Psu4,
    Slot::Psu5,
];

/// How long to wait after task startup before we start trying to inspect
/// things.
const STARTUP_SETTLE_MS: u64 = 500; // Current value is somewhat arbitrary.

/// How long to leave a PSU off on fault before attempting to re-enable it.
const FAULT_OFF_MS: u64 = 5_000; // Current value is somewhat arbitrary.

/// How long to wait after a PSU is inserted before entering the probation
/// state.
///
/// We wait to enable the MWOCP68 until after this time has past, which
/// does double-duty in both debouncing the presence line and ensuring that
/// things are firmly mated before activating anything.
///
/// The MWOCP67, however, automatically enables itself as soon as it's inserted.
/// The delay isn't as useful in that case, but is still good for debouncing the
/// presence line.
const INSERT_DEBOUNCE_MS: u64 = 1_000; // Current value is somewhat arbitrary.

/// How long after exiting a fault state before we require the PSU to start
/// asserting OK. Or, conversely, how long to ignore the OK output after
/// re-enabling a faulted PSU.
///
/// We have observed delays of up to 2s in practice (specifically after a dead
/// short on the mwocp67's main rail). Leaving the PSU enabled in a fault state
/// shouldn't be destructive, so we've padded this to avoid flapping.
const PROBATION_MS: u64 = 4000;

/// How often to poll presence (and back-stop the `PWR_OK` edge notification).
///
/// This should be fast enough to reliably spot removed sleds.
const POLL_MS: u64 = 500;

/// An action requested of the board-specific layer by the sequencing logic.
#[derive(Copy, Clone, PartialEq, Eq)]
#[must_use]
enum ActionRequired {
    /// The PSU has been hot-inserted and had time to settle, and should now be
    /// enabled (if it wasn't already enabled by default).
    EnableOnInsertion,
    /// The PSU has reported a fault and should be disabled as part of the fault
    /// recovery sequence.
    DisableOnFault,
    /// The PSU should now be re-enabled and any latched faults should be
    /// cleared, so it can (hopefully) resume operation.
    ReEnableAfterFault,
    /// The PSU has been hot-removed. If possible, it should be disabled so that
    /// it will not immediately turn on if re-inserted.
    DisableOnRemoval,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Present {
    #[default]
    No,
    Yes,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum PowerStatus {
    #[default]
    NotGood,
    Good,
}

#[derive(Copy, Clone)]
enum InitialState {
    /// Found present and apparently enabled at boot. It may still be powering
    /// up, so give it probation before trusting `PWR_OK`, but don't treat it as
    /// a new insertion.
    FoundEnabled,
    /// Found present but apparently forced off by a previous incarnation of
    /// this task. Resume fault recovery.
    FoundDisabled,
}

/// The ereporter, shared by all the PSU futures. It's set in [`main`] before
/// the executor starts. (It's an `Option` only because the ereporter can't be
/// built in a `const` context.)
static EREPORTER: StaticCell<Option<Ereporter>> = StaticCell::new(None);

/// Runs `f` with the shared ereporter.
///
/// `f` is synchronous, so the borrow can't be held across an `.await`, and
/// no two futures can ever borrow at once. Callers should do any I2C reads
/// they need for the ereport's fields before calling this.
fn with_ereporter(f: impl FnOnce(&mut Ereporter)) {
    f(EREPORTER.borrow_mut().as_mut().unwrap_lite());
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let sys = Sys::from(SYS.get_task_id());

    // The chassis LED is active high and pulled down by an external resistor.
    // If this is a task restart, our previous incarnation may have configured
    // the STATUS_LED pin as an output and turned the LED on.
    //
    // Turn it back off and reconfigure the pin (a no-op if it's already
    // configured).
    //
    // This sequence should not glitch in practice (though it also doesn't much
    // matter if we glitch an LED).
    sys.gpio_reset(bsp::STATUS_LED);
    sys.gpio_configure_output(
        bsp::STATUS_LED,
        OutputType::PushPull,
        Speed::Low,
        Pull::None,
    );

    // Populate packrat with our mac address and identity. Doing this now lets
    // the netstack wake up and start being useful while we're mucking around
    // with GPIOs below.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

    *EREPORTER.borrow_mut() = Some(Ereporter::claim_static_resources(packrat));

    let jefe = Jefe::from(JEFE.get_task_id());
    jefe.set_state(PowerState::A2 as u32);

    // Delay to allow things to settle, in case we were hot-plugged. This runs
    // *before* the executor starts, so a blocking sleep is fine and doesn't
    // interfere with any async task.
    #[allow(clippy::disallowed_methods)]
    hubris_async::danger::sleep_for(STARTUP_SETTLE_MS);

    // Now, configure the presence/OK detect nets. We want these to be inputs;
    // at power-on reset they're analog. Switching pins between those two modes
    // cannot glitch, and nobody would be listening if it did.
    sys.gpio_configure_input(bsp::ALL_PSU_PWR_OK_PINS, Pull::None);
    sys.gpio_configure_input(bsp::ALL_PSU_PRESENT_L_PINS, Pull::None);

    // Collect all of the pin-change notifications we want into a mask word, and
    // turn on pin-change notifications for all of our `PWR_OK` nets. The
    // `pin_sampler` future re-enables them before each wait.
    let all_pin_notifications = {
        let mut bits = 0;
        for mask in bsp::PSU_PWR_OK_NOTIF {
            bits |= mask;
        }
        bits
    };
    sys.gpio_irq_configure(all_pin_notifications, Edge::Both);

    // Read the pins to determine whether each PSU is present. This same sample
    // seeds `PINS` below, so that the PSU futures start from the presence
    // their initial states were chosen by. A PSU pulled after this is then
    // seen as a removal on the first fresh sample.
    let initial_pins = read_pins(&sys);
    let present = initial_pins.present;

    let mut devs: [bsp::Mwocp6x; PSU_COUNT] = core::array::from_fn(|i| {
        let i2c = I2C.get_task_id();
        let make_dev = bsp::PSU_PMBUS_DEVS[i];
        let (dev, opt_rail) = make_dev(i2c);
        let rail = opt_rail.unwrap_or(0);
        bsp::Mwocp6x::new(&dev, rail)
    });

    // Figure out whether any of the PSUs were previously disabled, so we can
    // seed each PSU future's starting point.
    let start_time = sys_get_timer().now;
    let initial_psu_enabled =
        bsp::initialize_enable_states(&sys, &mut devs, &present, start_time);

    let states: [Option<InitialState>; PSU_COUNT] = core::array::from_fn(|i| {
        if present[i] != Present::Yes {
            None
        } else if initial_psu_enabled[i] {
            Some(InitialState::FoundEnabled)
        } else {
            // PSU was forced off by our previous incarnation.
            Some(InitialState::FoundDisabled)
        }
    });

    // Turn the chassis LED on to indicate that we're alive.
    sys.gpio_set(bsp::STATUS_LED);
    // TODO: if we wanted to kick jefe into a greater-than-A2 state, this'd be
    // where it happens.

    // Seed the pin snapshot with the sample the initial states came from (see
    // above). `pin_sampler` logs `PWR_OK` changes relative to it.
    PINS.publish(initial_pins);

    // Hand each PSU (and its device) to its own future, and run them.
    let executor = Executor::new();
    for ((dev, slot), state) in devs.into_iter().zip(PSU_SLOTS).zip(states) {
        executor.spawn(psu_lifecycle(dev, slot, state));
    }
    executor.spawn(pin_sampler(all_pin_notifications));

    executor.run()
}

// ===========================================================================
// The pin sampler, and the per-PSU lifecycle futures. One lifecycle runs per
// PSU; together they are the sequencing state machine.
// ===========================================================================

/// The presence and `PWR_OK` pins of every PSU, read at one instant.
#[derive(Copy, Clone, PartialEq, Eq)]
struct Pins {
    present: [Present; PSU_COUNT],
    power_ok: [PowerStatus; PSU_COUNT],
}

impl Pins {
    /// Whether `slot`'s PSU is present but not asserting `PWR_OK`. `PWR_OK`
    /// can't be trusted without presence, so an absent PSU is never faulted.
    ///
    /// The presence check is defensive: the PSU futures only run on samples
    /// showing their PSU present (see [`monitor_until_removed`]).
    fn is_faulted(&self, slot: Slot) -> bool {
        self.present[slot as usize] == Present::Yes
            && self.power_ok[slot as usize] == PowerStatus::NotGood
    }
}

/// The latest [`Pins`] snapshot, published by [`pin_sampler`]. Every PSU
/// future learns about its pins from here, rather than reading GPIOs itself.
static PINS: Watch<Pins, PSU_COUNT> = Watch::new(Pins {
    present: [Present::No; PSU_COUNT],
    power_ok: [PowerStatus::NotGood; PSU_COUNT],
});

/// Samples every PSU's pins on any `PWR_OK` edge, and every `POLL_MS`
/// regardless, and publishes each sample to [`PINS`].
///
/// Presence and `PWR_OK` always come from the same instant, as in a
/// synchronous poll loop, and all six PSU futures share one set of GPIO reads
/// per sample. It also logs every `PWR_OK` change, whatever the PSU futures
/// are doing.
///
/// Every sample is published, even an unchanged one: some PSU futures (see
/// [`wait_for_fault`]) do periodic work on each one.
///
/// The level is sampled when we wake, so a pulse that comes and goes between
/// wakes isn't seen.
#[hubris_async::task]
async fn pin_sampler(all_pin_notifications: u32) {
    let sys = Sys::from(SYS.get_task_id());
    let mut last = PINS.get();
    loop {
        // Enable before reading, so an edge after the read always wakes us.
        sys.gpio_irq_control(all_pin_notifications, IrqControl::Enable)
            .unwrap_lite();
        let pins = read_pins(&sys);

        let now = now();
        for (i, &slot) in PSU_SLOTS.iter().enumerate() {
            if pins.power_ok[i] != last.power_ok[i] {
                ringbuf_entry!(
                    __TRACE,
                    Trace::power_good_changed(pins.power_ok[i], now, slot)
                );
            }
        }
        PINS.publish(pins);
        last = pins;

        select(
            notification::wait(all_pin_notifications),
            Timer::after(POLL_MS),
        )
        .await;
    }
}

/// The whole lifecycle of one PSU.
///
/// Waits for the PSU to be present, then runs [`monitor_present_psu`] racing a
/// single removal watch. A removal cancels the present side from whatever it
/// was doing; we handle it in one place here.
#[hubris_async::task(pool_size = PSU_COUNT)]
async fn psu_lifecycle(
    mut dev: bsp::Mwocp6x,
    slot: Slot,
    mut initial_state: Option<InitialState>,
) {
    let sys = Sys::from(SYS.get_task_id());
    // The PSU's FRU ID. We cache it here (rather than reading it at removal
    // time, when the PSU is gone) so we can include it in the removal ereport.
    let mut fruid = PsuFruid::default();

    loop {
        wait_for_presence(Present::Yes, slot).await;
        monitor_until_removed(
            &mut dev,
            slot,
            &sys,
            &mut fruid,
            initial_state.take(),
        )
        .await;
    }
}

/// One presence episode: everything that happens while the PSU is present,
/// raced against a single removal watch. [`monitor_present_psu`] never returns
/// on its own; the only way out is a removal cancelling it, handled here.
///
/// `select` polls the removal watch first, and both sides read the same
/// [`PINS`] sample within one poll (nothing can publish in between). So
/// whenever `monitor_present_psu` runs, the latest sample shows the PSU
/// present, and its `PWR_OK` reading is meaningful. Keep the removal watch as
/// the first argument.
async fn monitor_until_removed(
    dev: &mut bsp::Mwocp6x,
    slot: Slot,
    sys: &Sys,
    fruid: &mut PsuFruid,
    initial_state: Option<InitialState>,
) {
    match select(
        wait_for_presence(Present::No, slot),
        monitor_present_psu(dev, slot, sys, fruid, initial_state),
    )
    .await
    {
        Either::First(_) => {
            // The PSU was removed
            let now = now();
            ringbuf_entry!(Event::Removed { now, psu: slot });
            let fields = ereport_fields(dev, slot, *fruid);
            with_ereporter(|er| {
                let _ = er.deliver_ereport(&PsuRemovedEreport { fields });
            });
            run_action(ActionRequired::DisableOnRemoval, sys, slot, dev);
            // Clear the cached FRU ID only *after* it's in the ereport.
            *fruid = PsuFruid::default();
        }
        Either::Second(never) => match never {},
    }
}

/// The behavior of a PSU while it's present, as linear async control flow. This
/// future does not return; it's cancelled by the removal watch in
/// [`monitor_until_removed`].
async fn monitor_present_psu(
    dev: &mut bsp::Mwocp6x,
    slot: Slot,
    sys: &Sys,
    fruid: &mut PsuFruid,
    initial_state: Option<InitialState>,
) -> Infallible {
    match initial_state {
        Some(InitialState::FoundEnabled) => {
            // The task just started and found the PSU present and enabled
            fruid.refresh(dev, slot, now());
            ringbuf_entry!(Event::FoundEnabled {
                now: now(),
                psu: slot,
                serial: fruid.serial,
            });
            // Might still be powering on from a cold boot, so ignore PWR_OK for
            // a little while.
            Timer::after(PROBATION_MS).await;
        }
        Some(InitialState::FoundDisabled) => {
            // The task just started and found the PSU present and disabled
            fruid.refresh(dev, slot, now());
            ringbuf_entry!(Event::FoundAlreadyDisabled {
                now: now(),
                psu: slot,
                serial: fruid.serial,
            });
            // A previous incarnation forced this PSU off; resume recovery. It's
            // already disabled, so we skip straight to waiting it out and
            // re-enabling, emitting no new fault ereport.
            recover(dev, slot, sys, fruid).await;
        }

        None => {
            // The PSU has been inserted since the task started

            // Hello, who are you?
            // `fruid` should already be empty, but lets clear it just in case.
            *fruid = PsuFruid::default();
            fruid.refresh(dev, slot, now());
            ringbuf_entry!(Event::Inserted {
                now: now(),
                psu: slot,
                serial: fruid.serial,
            });

            // Debounce contact scrape. If the PSU bounces back out during this
            // wait, the removal watch cancels us.
            Timer::after(INSERT_DEBOUNCE_MS).await;
            // The latest sample may be up to `POLL_MS` old, so wait for a fresh
            // one before committing to the insertion. If the PSU was pulled
            // near the end of the debounce, the removal watch sees it first and
            // cancels us, rather than us reporting an insertion and enabling a
            // PSU that's already gone.
            PINS.changed().await;

            // Try once more to read the FRU ID, in case the I2C reads above
            // failed.
            fruid.refresh(dev, slot, now());
            let fields = ereport_fields(dev, slot, *fruid);
            with_ereporter(|er| {
                let _ = er.deliver_ereport(&PsuInsertedEreport { fields });
            });
            run_action(ActionRequired::EnableOnInsertion, sys, slot, dev);
            Timer::after(PROBATION_MS).await;
        }
    }

    // Steady state: be On until a fault, then report it, disable the PSU, and
    // recover; repeat.
    loop {
        wait_for_fault(dev, slot, fruid).await;
        disable_on_fault(dev, slot, sys, fruid);
        recover(dev, slot, sys, fruid).await;
    }
}

/// On: watch `PWR_OK` for a fault. Returns once it reads not-good.
///
/// Checks each [`PINS`] sample, which arrives on any `PWR_OK` edge and every
/// `POLL_MS` regardless.
///
/// A removal is never mistaken for a fault: the removal watch in
/// [`monitor_until_removed`] sees each sample first, and cancels us if it shows
/// the PSU absent.
async fn wait_for_fault(
    dev: &mut bsp::Mwocp6x,
    slot: Slot,
    fruid: &mut PsuFruid,
) {
    let mut next_refresh = 0;
    loop {
        // In case we couldn't read some FRU ID field earlier due to I2C
        // weather, try again. This is cheap once every field is populated,
        // but a field that keeps failing costs I2C retries each time, so do it
        // at most once per `POLL_MS` rather than on every sample (samples also
        // arrive on other PSUs' `PWR_OK` edges).
        let now = now();
        if now >= next_refresh {
            fruid.refresh(dev, slot, now);
            next_refresh = now + POLL_MS;
        }

        if PINS.get().is_faulted(slot) {
            return;
        }
        PINS.changed().await;
    }
}

/// Logs and reports a newly-observed fault, then turns the PSU off. Called once
/// when [`wait_for_fault`] sees `PWR_OK` drop, before [`recover`] takes over.
fn disable_on_fault(
    dev: &mut bsp::Mwocp6x,
    slot: Slot,
    sys: &Sys,
    fruid: &PsuFruid,
) {
    let now = now();
    ringbuf_entry!(__TRACE, Trace::FaultRecoveryStarted { now, psu: slot });
    let pmbus_status = read_pmbus_status(dev, slot, now);
    let fields = ereport_fields(dev, slot, *fruid);
    with_ereporter(|er| {
        let _ = er.deliver_ereport(&PowerUngoodEreport {
            fields,
            pmbus_status,
        });
    });
    run_action(ActionRequired::DisableOnFault, sys, slot, dev);
}

/// Brings an off PSU back: let it discharge, re-enable it, give it probation,
/// and check. Returns once it's good again; keeps cycling while it stays
/// ungood. The PSU is assumed already off on entry — either
/// [`disable_on_fault`] just disabled it, or a previous incarnation did.
async fn recover(
    dev: &mut bsp::Mwocp6x,
    slot: Slot,
    sys: &Sys,
    fruid: &mut PsuFruid,
) {
    loop {
        // Allow things to discharge.
        Timer::after(FAULT_OFF_MS).await;
        fruid.refresh(dev, slot, now());

        // Turn the PSU back on _without regard_ to the OK signal state, because
        // the PSU won't assert OK when it's off. We learned this the hard way;
        // see #1800.
        run_action(ActionRequired::ReEnableAfterFault, sys, slot, dev);
        Timer::after(PROBATION_MS).await;
        fruid.refresh(dev, slot, now());

        // The removal watch has already confirmed that this sample shows the
        // PSU present (see `monitor_until_removed`), so `PWR_OK` is meaningful.
        let pins = PINS.get();

        if pins.power_ok[slot as usize] == PowerStatus::Good {
            let now = now();
            ringbuf_entry!(
                __TRACE,
                Trace::FaultRecoveryFinished { now, psu: slot }
            );
            let pmbus_status = read_pmbus_status(dev, slot, now);
            let fields = ereport_fields(dev, slot, *fruid);
            with_ereporter(|er| {
                let _ = er.deliver_ereport(&PowerGoodEreport {
                    fields,
                    pmbus_status,
                });
            });
            return;
        }

        // Still ungood — turn it back off and try again.
        ringbuf_entry!(
            __TRACE,
            Trace::PowerStillUngood {
                now: now(),
                psu: slot,
            }
        );
        run_action(ActionRequired::DisableOnFault, sys, slot, dev);
    }
}

/// Waits until a [`PINS`] sample shows the PSU either present or not present,
/// and returns that sample.
///
/// Presence isn't interrupt-driven on either board yet (see hubris#2565), so
/// a change is seen within `POLL_MS`.
async fn wait_for_presence(desired_presence: Present, slot: Slot) -> Pins {
    PINS.wait_for(|pins| pins.present[slot as usize] == desired_presence)
        .await
}

/// Performs a board-specific action and records the result.
fn run_action(
    action: ActionRequired,
    sys: &Sys,
    slot: Slot,
    dev: &mut bsp::Mwocp6x,
) {
    let now = now();
    // The sequencing logic won't know if we failed to enable/disable the PSU as
    // it requested, so its progress could get out-of-sync with reality. But
    // this should resolve itself eventually: a failed disable-on-fault either
    // recovers anyway or re-faults and tries again; a failed enable leaves
    // PWR_OK deasserted, which triggers another fault recovery.
    let result = bsp::do_action(action, sys, slot as usize, dev, now);
    ringbuf_entry!(Event::action_result(action, result.err(), now, slot));
}

// ===========================================================================
// FRU ID, PMBus status, and ereport helpers.
// ===========================================================================

#[derive(Copy, Clone, Default, microcbor::Encode)]
struct PsuFruid {
    mfr: Option<FixedString<9>>,
    mpn: Option<FixedString<17>>,
    serial: Option<FixedString<12>>,
    fw_rev: Option<FixedString<4>>,
}

impl PsuFruid {
    fn refresh(&mut self, dev: &bsp::Mwocp6x, psu: Slot, now: u64) {
        if self.mfr.is_none() {
            self.mfr = retry_i2c_txn(now, psu, || dev.mfr_id())
                .ok()
                .and_then(|v| FixedString::try_from_utf8(&v.0[..]).ok());
        }

        if self.serial.is_none() {
            self.serial = retry_i2c_txn(now, psu, || dev.serial_number())
                .ok()
                .and_then(|v| FixedString::try_from_utf8(&v.0[..]).ok());
        }

        if self.mpn.is_none() {
            self.mpn = retry_i2c_txn(now, psu, || dev.model_number())
                .ok()
                .and_then(|v| FixedString::try_from_utf8(&v.0[..]).ok());
        }

        if self.fw_rev.is_none() {
            self.fw_rev = retry_i2c_txn(now, psu, || dev.firmware_revision())
                .ok()
                .and_then(|v| FixedString::try_from_utf8(&v.primary[..]).ok());
        }
    }
}

fn read_pmbus_status(
    dev: &bsp::Mwocp6x,
    slot: Slot,
    now: u64,
) -> ereports::pwr::PmbusStatus {
    let status_word =
        retry_i2c_txn(now, slot, || dev.status_word()).map(|data| data.0);
    ringbuf_entry!(
        __TRACE,
        Trace::StatusWord {
            psu: slot,
            now,
            status_word
        }
    );

    let status_iout =
        retry_i2c_txn(now, slot, || dev.status_iout()).map(|data| data.0);
    ringbuf_entry!(
        __TRACE,
        Trace::StatusIout {
            psu: slot,
            now,
            status_iout
        }
    );

    let status_vout =
        retry_i2c_txn(now, slot, || dev.status_vout()).map(|data| data.0);
    ringbuf_entry!(
        __TRACE,
        Trace::StatusVout {
            psu: slot,
            now,
            status_vout
        }
    );
    let status_input =
        retry_i2c_txn(now, slot, || dev.status_input()).map(|data| data.0);
    ringbuf_entry!(
        __TRACE,
        Trace::StatusInput {
            psu: slot,
            now,
            status_input,
        }
    );

    let status_cml =
        retry_i2c_txn(now, slot, || dev.status_cml()).map(|data| data.0);
    ringbuf_entry!(
        __TRACE,
        Trace::StatusCml {
            psu: slot,
            now,
            status_cml
        }
    );

    let status_temperature =
        retry_i2c_txn(now, slot, || dev.status_temperature())
            .map(|data| data.0);
    ringbuf_entry!(
        __TRACE,
        Trace::StatusTemperature {
            psu: slot,
            now,
            status_temperature
        }
    );

    let status_mfr_specific =
        retry_i2c_txn(now, slot, || dev.status_mfr_specific())
            .map(|data| data.0);
    ringbuf_entry!(
        __TRACE,
        Trace::StatusMfrSpecific {
            psu: slot,
            now,
            status_mfr_specific
        }
    );

    ereports::pwr::PmbusStatus {
        word: status_word.ok(),
        iout: status_iout.ok(),
        vout: status_vout.ok(),
        input: status_input.ok(),
        cml: status_cml.ok(),
        temp: status_temperature.ok(),
        mfr: status_mfr_specific.ok(),
    }
}

fn ereport_fields(
    dev: &bsp::Mwocp6x,
    slot: Slot,
    fruid: PsuFruid,
) -> EreportFields {
    let rail = {
        // This is a little silly, but it stops us from having to 6 separate
        // instances of the string "V54_PSU" in the binary...
        //
        // If you add a new name, make sure it still fits in EreportFields'
        // `rail` FixedString.
        #[cfg(any(target_board = "psc-b", target_board = "psc-c"))]
        let mut rail_name = *b"V54_PSUx";
        #[cfg(target_board = "observer-a")]
        let mut rail_name = *b"V50_PSUx";

        rail_name[rail_name.len() - 1] = match slot {
            Slot::Psu0 => b'0',
            Slot::Psu1 => b'1',
            Slot::Psu2 => b'2',
            Slot::Psu3 => b'3',
            Slot::Psu4 => b'4',
            Slot::Psu5 => b'5',
        };
        FixedString::try_from_utf8(&rail_name[..]).unwrap_lite()
    };
    EreportFields {
        refdes: FixedStr::from_str(dev.i2c_device().component_id()),
        rail,
        slot: slot as u8,
        fruid,
    }
}

fn retry_i2c_txn<T>(
    now: u64,
    psu: Slot,
    mut txn: impl FnMut() -> Result<T, mwocp6x::Error>,
) -> Result<T, mwocp6x::Error> {
    // Chosen by fair dice roll, seems reasonable-ish?
    let mut retries_remaining = 3;
    loop {
        match txn() {
            Ok(x) => return Ok(x),
            Err(err) => {
                ringbuf_entry!(
                    __I2C_TRACE,
                    I2cTrace::I2cError { now, psu, err }
                );

                if retries_remaining == 0 {
                    return Err(err);
                }

                retries_remaining -= 1;
            }
        }
    }
}

// Return the state indicated by each PSU's PRESENT_L signal
fn read_presence(sys: &Sys) -> [Present; PSU_COUNT] {
    let present_l_bits = sys.gpio_read(bsp::ALL_PSU_PRESENT_L_PINS);
    core::array::from_fn(|i| {
        // Presence signals are active LOW.
        if present_l_bits & (1 << bsp::PSU_PRESENT_L_PINS[i]) == 0 {
            Present::Yes
        } else {
            Present::No
        }
    })
}

// Return the state indicated by each PSU's OK signal.
fn read_power_ok(sys: &Sys) -> [PowerStatus; PSU_COUNT] {
    let ok_bits = sys.gpio_read(bsp::ALL_PSU_PWR_OK_PINS);
    core::array::from_fn(|i| {
        // PWR_OK signals are active HIGH.
        if ok_bits & (1 << bsp::PSU_PWR_OK_PINS[i]) != 0 {
            PowerStatus::Good
        } else {
            PowerStatus::NotGood
        }
    })
}

// Read every PSU's presence and OK signals.
fn read_pins(sys: &Sys) -> Pins {
    Pins {
        present: read_presence(sys),
        power_ok: read_power_ok(sys),
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

ereports::declare_ereporter! {
    struct Ereporter<Ereport> {
        PsuInserted(PsuInsertedEreport),
        PsuRemoved(PsuRemovedEreport),
        PowerGood(PowerGoodEreport),
        PowerUngood(PowerUngoodEreport)
    }
}

#[derive(microcbor::Encode)]
#[ereport(class = "hw.insert.psu", version = 0)]
struct PsuInsertedEreport {
    #[cbor(flatten)]
    fields: EreportFields,
}

#[derive(microcbor::Encode)]
#[ereport(class = "hw.remove.psu", version = 0)]
struct PsuRemovedEreport {
    #[cbor(flatten)]
    fields: EreportFields,
}

#[derive(microcbor::Encode)]
#[ereport(class = "hw.pwr.pwr_good.good", version = 0)]
struct PowerGoodEreport {
    #[cbor(flatten)]
    fields: EreportFields,
    pmbus_status: ereports::pwr::PmbusStatus,
}

#[derive(microcbor::Encode)]
#[ereport(class = "hw.pwr.pwr_good.bad", version = 0)]
struct PowerUngoodEreport {
    #[cbor(flatten)]
    fields: EreportFields,
    pmbus_status: ereports::pwr::PmbusStatus,
}

#[derive(microcbor::EncodeFields)]
struct EreportFields {
    refdes: FixedStr<'static, 20>, // Component ID max length
    rail: FixedString<8>,          // Example: "V54_PSU0"
    slot: u8,
    fruid: PsuFruid,
}
