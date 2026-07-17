// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the PSC sequencing process.
//!
//! # Hardware Support
//!
//! The server supports both the original PSC's MWOCP68 PSU and the Observer's MWOCP67
//! PSU, which have some annoying differences:
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
//! At that point, we can start our main management loop, which continuously
//! does the following for each power supply separately:
//!
//! - Watch for the presence line to be high, meaning the PSU is removed.
//! - Record that the PSU is missing.
//! - Start driving its ON signal high. (MWOCP68 only)
//! - Wait for the presence line to be low, meaning the PSU is reinserted.
//! - Release its ON signal so it may turn on normally. (MWOCP68 only)
//!
//! Simultaneously, while the PSU is not removed, we monitor the OK signal for
//! indication of internal faults, and periodically poll PMBus status registers
//! for faults that may not be indicated through the OK signal. (The behavior of
//! the OK signal is not super clear from Murata's documentation.) If we find a
//! fault, we...
//!
//! - Record as much information as we can reasonably gather.
//! - Force the PSU off, using the ON signal or a PMBus command.
//! - Wait some time to allow things to discharge.
//! - Turn the PSU back on.
//! - Wait some time for it to wake.
//! - Start watching the fault signal again.
//!
//! Removing and reinserting a PSU in general clears the fault state _and_
//! resets the retry counter.
//!
//!
//! # Generalizing to task restarts
//!
//! This task is not intended to restart under normal operation, but bugs
//! happen. We can attempt to maintain glitch-free (or at least low-glitch)
//! operation in the face of this task crashing by doing the following:
//!
//! At task startup, try to determine whether any of the PSUs were in the middle
//! of a fault recovery sequence (above). If a PSU appears to have been turned
//! off by our previous incarnation, begin a fresh fault recovery sequence on
//! that PSU as if we had newly detected a fault.
//!
//! Task crashes will reset the fault counter and timeout. This is unavoidable
//! without keeping state across incarnations, which we're trying to avoid to
//! reduce the likelihood of crashloops.
//!
//! Task crashes may also reactivate a PSU that the control plane had commanded
//! off. Currently this is unavoidable; we might want to record such overrides
//! in the FRAM to be safe.

#![no_std]
#![no_main]

use drv_i2c_devices::mwocp6x;

use drv_packrat_vpd_loader::{Packrat, read_vpd_and_load_packrat};
use drv_psc_seq_api::PowerState;
use drv_stm32xx_sys_api as sys_api;
use sys_api::{Edge, IrqControl, Pull};
use task_jefe_api::Jefe;
use userlib::{
    UnwrapLite, hl, sys_get_timer, sys_recv_notification, sys_set_timer,
    task_slot,
};

use fixedstr::{FixedStr, FixedString};
use ringbuf::{counted_ringbuf, ringbuf_entry};

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
    /// Emitted after sucessfully doing an enable/disable action that was
    /// requested by the state machine.
    ActionSucceeded {
        action: ActionRequired,
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Emitted after failing to do an enable/disable action that was requested
    /// by the state machine.
    ActionFailed {
        action: ActionRequired,
        now: u64,
        #[count(children)]
        psu: Slot,
        err: mwocp6x::Error,
    },
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
/// Since exactly one of each register entry is recorded for every `Faulted` and
/// `FaultCleared` entry, we don't really need to spend extra bytes on counting
/// them, so they are marked as `count(skip)`.
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
    /// The new state of the PWR_OK pin.
    PowerGoodChanged {
        now: u64,
        status: Status,
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
    I2cError {
        now: u64,
        #[count(children)]
        psu: Slot,
        err: mwocp6x::Error,
    },
}

// Since entries in this ringbuffer contain timestamps, they will never be
// de-duplicated. Thus, disable it.
counted_ringbuf!(__TRACE, Trace, 32, Trace::None, no_dedup);

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

/// How long to wait after a PSU is inserted, before we attempt to turn it on
/// (MWOCP68 only). This does double-duty in both debouncing the presence line,
/// and ensuring that things are firmly mated before activating anything.
const INSERT_DEBOUNCE_MS: u64 = 1_000; // Current value is somewhat arbitrary.

/// How long after exiting a fault state before we require the PSU to start
/// asserting OK. Or, conversely, how long to ignore the OK output after
/// re-enabling a faulted PSU.
///
/// We have observed delays of up to 2s in practice (specifically after a dead
/// short on the mwocp67's main rail). Leaving the PSU enabled in a fault state
/// shouldn't be destructive, so we've padded this to avoid flapping.
const PROBATION_MS: u64 = 4000;

/// How often to check the status of polled inputs.
///
/// This should be fast enough to reliably spot removed sleds.
const POLL_MS: u64 = 500;

/// An action requested by the state machine.
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

#[derive(Copy, Clone)]
enum PsuState {
    /// The PSU is detected as not present. In this state, we cannot trust the
    /// OK signal.
    NotPresent,
    /// The PSU is detected as present.
    Present(PresentState),
}

#[derive(Copy, Clone)]
enum PresentState {
    /// The PSU is enabled.
    ///
    /// This is the initial state upon either detecting a new PSU, or power
    /// up/restart in cases where the PSU is not forced off.
    ///
    /// We will exit this state if the OK line is pulled low, or if we detect a
    /// fault.
    On {
        /// If `true`, the PSU was power-cycled by the PSC in attempt to clear a
        /// fault. If it reasserts `PWR_OK`, that indicates that the fault has
        /// cleared; otherwise, the fault is persistent.
        ///
        /// If `false`, the PSU was either newly inserted, or a previous fault
        /// has cleared. A new fault should produce a new fault ereport.
        was_faulted: bool,
    },

    /// The PSU has just appeared and we're waiting a bit to confirm that it's
    /// stable before turning it on (MWOCP68 only). (Waiting in this state
    /// provides some debouncing for contact scrape.)
    NewlyInserted { settle_deadline: u64 },
    /// The PSU has unexpectedly deasserted the OK signal, or failed to assert
    /// it within a reasonable amount of time after being turned on.
    Faulted {
        // Try to turn the PSU back on when this time is reached.
        turn_on_deadline: u64,
    },

    /// The PSU is enabled, as in the `On` state, but we're not convinced the
    /// PSU is okay. We enter this state when the PSU is first inserted and when
    /// bringing a PSU out of an observed fault state, and it causes us to
    /// ignore its OK output for a brief period (the deadline parameter,
    /// initialized as current time plus `PROBATION_MS`).
    ///
    /// We do this because PSUs have been observed, in practice, taking up to 2s
    /// to assert OK after being enabled. The MWOCP67 is much slower to assert
    /// OK than the MWOCP68 is.
    ///
    /// Once the deadline elapses, we'll transition to the `On` state and start
    /// requiring OK to be asserted.
    OnProbation {
        deadline: u64,
        reason: ProbationReason,
    },
}

#[derive(Copy, Clone)]
enum ProbationReason {
    /// The PSU might not have asserted PWR_OK yet because it was just
    /// hot-inserted and enabled.
    Insertion,
    /// The PSU might not have asserted PWR_OK yet because it was just
    /// re-enabled after a fault.
    Fault,
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    let sys = sys_api::Sys::from(SYS.get_task_id());

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
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );

    // Populate packrat with our mac address and identity. Doing this now lets
    // the netstack wake up and start being useful while we're mucking around
    // with GPIOs below.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

    let mut ereporter = Ereporter::claim_static_resources(packrat);

    let jefe = Jefe::from(JEFE.get_task_id());
    jefe.set_state(PowerState::A2 as u32);

    // Delay to allow things to settle, in case we were hot-plugged.
    hl::sleep_for(STARTUP_SETTLE_MS);

    // Now, configure the presence/OK detect nets. We want these to be inputs;
    // at power-on reset they're analog. Switching pins between those two modes
    // cannot glitch, and nobody would be listening if it did.
    sys.gpio_configure_input(bsp::ALL_PSU_PWR_OK_PINS, Pull::None);
    sys.gpio_configure_input(bsp::ALL_PSU_PRESENT_L_PINS, Pull::None);

    // Collect all of the pin-change notifications we want into a mask word.
    // We'll use this each time we want to listen for pins.
    let all_pin_notifications = {
        let mut bits = 0;
        for mask in bsp::PSU_PWR_OK_NOTIF {
            bits |= mask;
        }
        bits
    };

    // Turn on pin change notifications on all of our input nets.
    sys.gpio_irq_configure(all_pin_notifications, Edge::Both);

    // Set up our state machines for each PSU. First we'll need to read the
    // presence pins to determine whether a PSU is present and if we should ask
    // it for its serial number.
    let present = read_presence(&sys);

    let mut devs: [bsp::Mwocp6x; PSU_COUNT] = core::array::from_fn(|i| {
        let i2c = I2C.get_task_id();
        let make_dev = bsp::PSU_PMBUS_DEVS[i];
        let (dev, opt_rail) = make_dev(i2c);
        let rail = opt_rail.unwrap_or(0);
        bsp::Mwocp6x::new(&dev, rail)
    });

    // We'll also need to know whether any of the PSUs were previously disabled.
    let start_time = sys_get_timer().now;
    let initial_psu_enabled =
        bsp::initialize_enable_states(&sys, &mut devs, &present, start_time);

    // Create the Psu objects, picking their initial states and giving them
    // ownership of the PMBus devices we constructed above.
    let mut devs = devs.into_iter();
    let mut psus: [Psu; PSU_COUNT] = core::array::from_fn(|i| {
        let dev = devs.next().unwrap_lite();
        let slot = PSU_SLOTS[i];
        let mut fruid = PsuFruid::default();
        let state = if present[i] == Present::Yes {
            // Hello, who are you?
            fruid.refresh(&dev, slot, start_time);
            // ...and how are you doing?
            PsuState::Present(if initial_psu_enabled[i] {
                ringbuf_entry!(Event::FoundEnabled {
                    now: start_time,
                    psu: slot,
                    serial: fruid.serial
                });
                PresentState::On { was_faulted: false }
            } else {
                // PSU was forced off by our previous incarnation. Schedule it to
                // turn back on in the future if things clear up.
                ringbuf_entry!(Event::FoundAlreadyDisabled {
                    now: start_time,
                    psu: slot,
                    serial: fruid.serial
                });
                PresentState::Faulted {
                    turn_on_deadline: start_time.saturating_add(FAULT_OFF_MS),
                }
            })
        } else {
            PsuState::NotPresent
        };
        Psu {
            slot,
            state,
            dev,
            fruid,
        }
    });

    // Turn the chassis LED on to indicate that we're alive.
    sys.gpio_set(bsp::STATUS_LED);
    // TODO: if we wanted to kick jefe into a greater-than-A2 state, this'd be
    // where it happens.

    // For logging when a PWR_OK pin changes state
    let mut last_ok: [Status; PSU_COUNT] = read_power_ok(&sys);

    // Poll things.
    sys_set_timer(Some(start_time), notifications::TIMER_MASK);
    let sleep_notifications = all_pin_notifications | notifications::TIMER_MASK;
    loop {
        sys.gpio_irq_control(all_pin_notifications, IrqControl::Enable)
            .unwrap_lite();

        let present = read_presence(&sys);
        let ok = read_power_ok(&sys);

        let now = sys_get_timer().now;
        for i in 0..PSU_COUNT {
            // Log if the PWR_OK pin changed state.
            if ok[i] != last_ok[i] {
                ringbuf_entry!(
                    __TRACE,
                    Trace::PowerGoodChanged {
                        now,
                        status: ok[i],
                        psu: PSU_SLOTS[i],
                    }
                );
            }

            // Step the state machine, asking it if there's any action we should
            // take.
            if let Some(action) =
                psus[i].step(now, present[i], ok[i], &mut ereporter)
            {
                if let Err(err) =
                    bsp::do_action(action, &sys, i, &mut psus[i].dev, now)
                {
                    // The state machine doesn't know that we failed to
                    // enable/disable the PSU as it requested, so its state will
                    // be out-of-sync with reality. But this should resolve
                    // itself eventually. If we fail to disable the PSU after a
                    // fault, then either the fault recovery sequence will work
                    // despite that, or it won't and we'll start another fault
                    // recovery sequence. If we fail to enable the PSU at the
                    // end of fault recovery, the deasserted OK signal will
                    // trigger another fault recovery sequence soon.
                    ringbuf_entry!(Event::ActionFailed {
                        action,
                        now,
                        psu: PSU_SLOTS[i],
                        err,
                    });
                } else {
                    ringbuf_entry!(Event::ActionSucceeded {
                        action,
                        now,
                        psu: PSU_SLOTS[i],
                    });
                }
            }
        }
        last_ok = ok;

        // Wait for a pin change or timer.
        let n = sys_recv_notification(sleep_notifications);
        // If the timer bit is set _and the timer has actually fired_...
        if n.has_timer_fired(notifications::TIMER_MASK) {
            // Reset our timer forward.
            sys_set_timer(
                Some(now.saturating_add(POLL_MS)),
                notifications::TIMER_MASK,
            );
        }
        // Ignore pin change notification bits, we just handle all the pins
        // above. We also _enable_ the pin change interrupts at the top of the
        // loop.
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Present {
    #[default]
    No,
    Yes,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Status {
    #[default]
    NotGood,
    Good,
}

struct Psu {
    slot: Slot,
    state: PsuState,
    dev: bsp::Mwocp6x,
    /// Because we would like to include the PSU's FRU ID information in the
    /// ereports generated when a PSU is *removed*, we must cache it here rather
    /// than reading it from the device when we generate an ereport for it.
    fruid: PsuFruid,
}

impl Psu {
    /// Advances the PSU management state machine given the current time (`now`)
    /// and the state of the `present` and `pwr_ok` inputs.
    ///
    /// This may be called at unpredictable intervals, and may be called more
    /// than once for the same timestamp value. The implementation **must** use
    /// `now` and the timer to control any time-sensitive operations.
    fn step(
        &mut self,
        now: u64,
        present: Present,
        pwr_ok: Status,
        ereporter: &mut Ereporter,
    ) -> Option<ActionRequired> {
        match (self.state, present, pwr_ok) {
            (PsuState::NotPresent, Present::No, _) => {
                // ignore the power good line, it is meaningless.
                None
            }

            // Regardless of our current state, if we observe the present line
            // low, treat the PSU as having been disconnected.
            //
            // Other than detecting removal, the main side effect of this
            // decision is that the "NewlyInserted" settle time starts after the
            // contacts are _done_ scraping, not when they start.
            (PsuState::Present(_), Present::No, _) => {
                ringbuf_entry!(Event::Removed {
                    now,
                    psu: self.slot
                });
                let _ = ereporter.deliver_ereport(&PsuRemovedEreport {
                    fields: self.ereport_fields(),
                });

                self.state = PsuState::NotPresent;
                // Clear the FRUID serial only *after* we have put it in the ereport.
                self.fruid = PsuFruid::default();

                Some(ActionRequired::DisableOnRemoval)
            }

            // In a not-present situation we have to ignore the OK line entirely
            // and only watch for the presence line to indicate the PSU has
            // appeared.
            (PsuState::NotPresent, Present::Yes, _) => {
                let settle_deadline = now.wrapping_add(INSERT_DEBOUNCE_MS);
                self.state = PsuState::Present(PresentState::NewlyInserted {
                    settle_deadline,
                });
                // Hello, who are you?
                self.fruid = PsuFruid::default();
                self.refresh_fruid(now);
                ringbuf_entry!(Event::Inserted {
                    now,
                    psu: self.slot,
                    serial: self.fruid.serial
                });
                // No external action required until our timer elapses.
                None
            }

            (
                PsuState::Present(PresentState::NewlyInserted {
                    settle_deadline,
                }),
                Present::Yes,
                _,
            ) => {
                // Hello, who are you?
                self.refresh_fruid(now);
                if settle_deadline <= now {
                    // The PSU is still present (since the Present::No case
                    // above didn't fire) and our deadline has elapsed. Let's
                    // treat this as a real insertion! It might take some more
                    // time for PWR_OK to be asserted, so we start in the
                    // OnProbation state.
                    self.state = PsuState::Present(PresentState::OnProbation {
                        deadline: now.wrapping_add(PROBATION_MS),
                        reason: ProbationReason::Insertion,
                    });
                    let _ = ereporter.deliver_ereport(&PsuInsertedEreport {
                        fields: self.ereport_fields(),
                    });

                    Some(ActionRequired::EnableOnInsertion)
                } else {
                    // Remain in this state.
                    None
                }
            }

            // yay!
            (
                PsuState::Present(PresentState::On { was_faulted }),
                Present::Yes,
                Status::Good,
            ) => {
                // Just in case we were previously unable to read any FRUID
                // values due to I2C weather, try to refresh them
                self.refresh_fruid(now);

                // If we just turned this PSU back on after a fault, reasserting
                // POWER_GOOD means that the fault has cleared.
                if was_faulted {
                    // Clear our tracking of the fault. If we fault again, treat
                    // that as a new fault.
                    self.state = PsuState::Present(PresentState::On {
                        was_faulted: false,
                    });
                    ringbuf_entry!(
                        __TRACE,
                        Trace::FaultRecoveryFinished {
                            now,
                            psu: self.slot,
                        }
                    );
                    // Report that the fault has gone away.
                    let _ = ereporter.deliver_ereport(&PowerGoodEreport {
                        pmbus_status: self.read_pmbus_status(now),
                        fields: self.ereport_fields(),
                    });
                }

                None
            }
            (
                PsuState::Present(PresentState::On { was_faulted }),
                Present::Yes,
                Status::NotGood,
            ) => {
                // The PSU appears to have pulled the OK signal into the "not
                // OK" state to indicate an internal fault!

                let turn_on_deadline = now.wrapping_add(FAULT_OFF_MS);
                self.state = PsuState::Present(PresentState::Faulted {
                    turn_on_deadline,
                });
                // Did we just restart after a fault? If not, this is a new
                // fault, which should be reported.
                if !was_faulted {
                    ringbuf_entry!(
                        __TRACE,
                        Trace::FaultRecoveryStarted {
                            now,
                            psu: self.slot,
                        }
                    );
                    let _ = ereporter.deliver_ereport(&PowerUngoodEreport {
                        fields: self.ereport_fields(),
                        pmbus_status: self.read_pmbus_status(now),
                    });
                } else {
                    ringbuf_entry!(
                        __TRACE,
                        Trace::PowerStillUngood {
                            now,
                            psu: self.slot,
                        }
                    );
                };

                Some(ActionRequired::DisableOnFault)
            }

            (
                PsuState::Present(PresentState::Faulted { turn_on_deadline }),
                Present::Yes,
                _,
            ) => {
                if turn_on_deadline <= now {
                    // We turn the PSU back on _without regard_ to the OK signal
                    // state, because the PSU won't assert OK when it's off! We
                    // learned this the hard way. See #1800.
                    self.state = PsuState::Present(PresentState::OnProbation {
                        deadline: now.saturating_add(PROBATION_MS),
                        reason: ProbationReason::Fault,
                    });
                    Some(ActionRequired::ReEnableAfterFault)
                } else {
                    None
                }
            }
            (
                PsuState::Present(PresentState::OnProbation {
                    deadline,
                    reason,
                }),
                Present::Yes,
                _,
            ) => {
                // Just in case we were previously unable to read any FRUID
                // values due to I2C weather, try to refresh them
                self.refresh_fruid(now);
                if deadline <= now {
                    // Take PSU out of probation state and start monitoring its
                    // OK line.
                    self.state = PsuState::Present(PresentState::On {
                        was_faulted: matches!(reason, ProbationReason::Fault),
                    });
                    None
                } else {
                    // Remain in this state.
                    None
                }
            }
        }
    }

    fn refresh_fruid(&mut self, now: u64) {
        self.fruid.refresh(&self.dev, self.slot, now);
    }

    fn read_pmbus_status(&mut self, now: u64) -> ereports::pwr::PmbusStatus {
        let status_word =
            retry_i2c_txn(now, self.slot, || self.dev.status_word())
                .map(|data| data.0);
        ringbuf_entry!(
            __TRACE,
            Trace::StatusWord {
                psu: self.slot,
                now,
                status_word
            }
        );

        let status_iout =
            retry_i2c_txn(now, self.slot, || self.dev.status_iout())
                .map(|data| data.0);
        ringbuf_entry!(
            __TRACE,
            Trace::StatusIout {
                psu: self.slot,
                now,
                status_iout
            }
        );

        let status_vout =
            retry_i2c_txn(now, self.slot, || self.dev.status_vout())
                .map(|data| data.0);
        ringbuf_entry!(
            __TRACE,
            Trace::StatusVout {
                psu: self.slot,
                now,
                status_vout
            }
        );
        let status_input =
            retry_i2c_txn(now, self.slot, || self.dev.status_input())
                .map(|data| data.0);
        ringbuf_entry!(
            __TRACE,
            Trace::StatusInput {
                psu: self.slot,
                now,
                status_input,
            }
        );

        let status_cml =
            retry_i2c_txn(now, self.slot, || self.dev.status_cml())
                .map(|data| data.0);
        ringbuf_entry!(
            __TRACE,
            Trace::StatusCml {
                psu: self.slot,
                now,
                status_cml
            }
        );

        let status_temperature =
            retry_i2c_txn(now, self.slot, || self.dev.status_temperature())
                .map(|data| data.0);
        ringbuf_entry!(
            __TRACE,
            Trace::StatusTemperature {
                psu: self.slot,
                now,
                status_temperature
            }
        );

        let status_mfr_specific =
            retry_i2c_txn(now, self.slot, || self.dev.status_mfr_specific())
                .map(|data| data.0);
        ringbuf_entry!(
            __TRACE,
            Trace::StatusMfrSpecific {
                psu: self.slot,
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

    fn ereport_fields(&self) -> EreportFields {
        let rail = {
            // This is a little silly, but it stops us from having to 6 separate
            // instances of the string "V54_PSU" in the binary...
            //
            // If you add a new name, make sure it still fits in EreportFields'
            // `rail` FixedString.
            #[cfg(any(target_board = "psc-b", target_board = "psc-c"))]
            let mut rail_name = *b"V54_PSUx";
            #[cfg(target_board = "observer-a")]
            let mut rail_name = *b"V50_MAIN_PSUx";

            rail_name[rail_name.len() - 1] = match self.slot {
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
            refdes: FixedStr::from_str(self.dev.i2c_device().component_id()),
            rail,
            slot: self.slot as u8,
            fruid: self.fruid,
        }
    }
}

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
                .and_then(|v| FixedString::try_from_utf8(&v.0[..]).ok());
        }
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
                ringbuf_entry!(__TRACE, Trace::I2cError { now, psu, err });

                if retries_remaining == 0 {
                    return Err(err);
                }

                retries_remaining -= 1;
            }
        }
    }
}

// Return the state indicated by each PSU's PRESENT_L signal
fn read_presence(sys: &sys_api::Sys) -> [Present; PSU_COUNT] {
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

// Return the state indicated by each PSU's OK signal
fn read_power_ok(sys: &sys_api::Sys) -> [Status; PSU_COUNT] {
    let ok_bits = sys.gpio_read(bsp::ALL_PSU_PWR_OK_PINS);
    core::array::from_fn(|i| {
        // PWR_OK signals are active HIGH.
        if ok_bits & (1 << bsp::PSU_PWR_OK_PINS[i]) != 0 {
            Status::Good
        } else {
            Status::NotGood
        }
    })
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
    rail: FixedString<13>,         // Example: "V54_PSU0"
    slot: u8,
    fruid: PsuFruid,
}
