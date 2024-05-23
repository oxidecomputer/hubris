// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the PSC sequencing process.
//!
//!
//! # General notes on PSC power supply sequencing
//!
//! There are rules to follow here to avoid glitching the power supplies,
//! because glitching the power supplies here will glitch the entire rack,
//! making you very unpopular very quickly.
//!
//! **`ON_L` signals to the PSUs:** we normally leave our pins high-impedance on
//! these nets, allowing external resistors to pull them low. We only drive them
//! to _disable_ the PSU by driving it high. To achieve this, we leave the pin
//! configured as an input, pre-load the output value as "high," and toggle its
//! mode register between input and output.
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
//! we don't override this when the PSU is reinserted. So, at startup, the PSC
//! must leave the ON lines undriven, allowing them to float low.
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
//! - Watch for the presence line to be high (PSU removed).
//! - Record that the PSU is missing.
//! - Start driving its ON signal high.
//! - Wait for the presence line to be low (PSU reinserted).
//! - Release its ON signal so it may turn on normally.
//!
//! Simultaneously, while the PSU is not removed, we monitor the OK signal for
//! indication of internal faults, and periodically poll PMBus status registers
//! for faults that may not be indicated through the OK signal. (The behavior of
//! the OK signal is not super clear from Murata's documentation.) If we find a
//! fault, we...
//!
//! - Record as much information as we can reasonably gather.
//! - Start driving the ON signal high to force the PSU off.
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
//! At task startup, read the status of the ON output pins. If we find that one
//! of the PSUs is off, assume that we turned it off in a previous incarnation
//! before losing state. Begin a fault recovery sequence (above) on that PSU as
//! if we had newly detected a fault.
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

use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_psc_seq_api::PowerState;
use drv_stm32xx_sys_api as sys_api;
use sys_api::{Edge, IrqControl, OutputType, PinSet, Pull, Speed};
use task_jefe_api::Jefe;
use userlib::*;

use ringbuf::{ringbuf, ringbuf_entry};

task_slot!(SYS, sys);
task_slot!(I2C, i2c_driver);
task_slot!(JEFE, jefe);
task_slot!(PACKRAT, packrat);

#[derive(Copy, Clone, PartialEq, Eq)]
enum Trace {
    Empty,
    /// Emitted at task startup when we find that a power supply is probably
    /// already on. (Note that if the power supply is not present, we will still
    /// detect it as "on" due to the pull resistors.)
    FoundEnabled {
        psu: u8,
    },
    /// Emitted at task startup when we find that a power supply appears to have
    /// been disabled.
    FoundAlreadyDisabled {
        psu: u8,
    },
    /// Emitted when we decide a power supply should be on.
    Enabling {
        psu: u8,
    },
    /// Emitted when we decide a power supply should be off; the `present` flag
    /// means the PSU is being turned off despite being present (`true`) or is
    /// being disabled because it's been removed (`false`).
    Disabling {
        psu: u8,
        present: bool,
    },
}

ringbuf!((u64, Trace), 128, (0, Trace::Empty));

const STATUS_LED: sys_api::PinSet = sys_api::Port::A.pin(3);

// The per-PSU signal definitions below all refer to this constant for the
// number of PSUs. It's not intended to be easily configurable, since that'd
// require hardware changes.
const PSU_COUNT: usize = 6;

// The ON signals are conveniently all routed to a single port:
const PSU_ENABLE_L_PORT: sys_api::Port = sys_api::Port::K;
// The ON signals are routed to the following pins on their port:
const PSU_ENABLE_L_PINS: [usize; PSU_COUNT] = [0, 1, 2, 3, 4, 5];
// Convenient mask for referring to all the ON pins simultaneously, since we can
// do that, since they're all on one port.
const ALL_PSU_ENABLE_L_PINS: sys_api::PinSet =
    PSU_ENABLE_L_PORT.pins(PSU_ENABLE_L_PINS);

// The PRESENT signals are conveniently all routed to a single port:
const PSU_PRESENT_L_PORT: sys_api::Port = sys_api::Port::J;
// The PRESENT signals are routed to the following pins on their port:
const PSU_PRESENT_L_PINS: [usize; PSU_COUNT] = [0, 1, 2, 3, 4, 5];
// Convenient mask for referring to all the PRESENT pins simultaneously, since
// we can do that, since they're all on one port.
const ALL_PSU_PRESENT_L_PINS: sys_api::PinSet =
    PSU_PRESENT_L_PORT.pins(PSU_PRESENT_L_PINS);

// The `PWR_OK` signals are conveniently all routed to a single port:
const PSU_PWR_OK_PORT: sys_api::Port = sys_api::Port::J;
// The `PWR_OK` signals are routed to the following pins on their port:
const PSU_PWR_OK_PINS: [usize; PSU_COUNT] = [6, 7, 8, 9, 10, 11];
// Convenient mask for referring to all the `PWR_OK` pins simultaneously, since
// we can do that, since they're all on one port.
const ALL_PSU_PWR_OK_PINS: sys_api::PinSet =
    PSU_PWR_OK_PORT.pins(PSU_PWR_OK_PINS);

// Our notification configuration system doesn't have any concept of arrays, so,
// collect its predefined masks into convenient arrays.
const PSU_PWR_OK_NOTIF: [u32; PSU_COUNT] = [
    notifications::PSU_PWR_OK_1_MASK,
    notifications::PSU_PWR_OK_2_MASK,
    notifications::PSU_PWR_OK_3_MASK,
    notifications::PSU_PWR_OK_4_MASK,
    notifications::PSU_PWR_OK_5_MASK,
    notifications::PSU_PWR_OK_6_MASK,
];

/// How long to wait after task startup before we start trying to inspect
/// things.
const STARTUP_SETTLE_MS: u64 = 500; // Current value is somewhat arbitrary.

/// How long to leave a PSU off on fault before attempting to re-enable it.
const FAULT_OFF_MS: u64 = 5_000; // Current value is somewhat arbitrary.

/// How long to wait after a PSU is inserted, before we attempt to turn it on.
/// This does double-duty in both debouncing the presence line, and ensuring
/// that things are firmly mated before activating anything.
const INSERT_DEBOUNCE_MS: u64 = 1_000; // Current value is somewhat arbitrary.

/// How often to check the status of polled inputs.
///
/// This should be fast enough to reliably spot removed sleds.
const POLL_MS: u64 = 500;

#[derive(Copy, Clone)]
#[must_use]
enum ActionRequired {
    /// Requests that this PSU be enabled by setting the corresponding
    /// `ENABLE_L` low.
    EnableMe,
    /// Requests that this PSU be disabled by setting the corresponding
    /// `ENABLE_L` high. `attempt_snapshot` will be `true` if the PSU is
    /// believed to still be present and recording data may be useful, or
    /// `false` if the PSU is believed removed and isn't worth polling.
    DisableMe { attempt_snapshot: bool },
}

#[derive(Copy, Clone)]
enum PsuState {
    /// The PSU is detected as not present. In this state, we cannot trust the
    /// OK signal, and we deassert the ENABLE signal.
    NotPresent,
    /// The PSU is detected as present. We assume this at powerup until proven
    /// otherwise.
    Present(PresentState),
}

#[derive(Copy, Clone)]
enum PresentState {
    /// We are allowing the ON signal to float active (low).
    ///
    /// This is the initial state upon either detecting a new PSU, or power
    /// up/restart in cases where the PSU is not forced off.
    ///
    /// We will exit this state if the OK line is pulled low, or if we detect a
    /// fault.
    On,

    /// The PSU has just appeared and we're waiting a bit to confirm that it's
    /// stable before turning it on. (Waiting in this state provides some
    /// debouncing for contact scrape.)
    NewlyInserted { settle_deadline: u64 },
    /// The PSU has unexpectedly deasserted the OK signal, or failed to assert
    /// it within a reasonable amount of time after being turned on.
    Faulted {
        // Try to turn the PSU back on when this time is reached, but only if
        // the fault has cleared. Otherwise, we will stay in the fault state
        // with a "sticky fault" situation.
        turn_on_deadline: u64,
    },
}

#[export_name = "main"]
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
    sys.gpio_reset(STATUS_LED);
    sys.gpio_configure_output(
        STATUS_LED,
        sys_api::OutputType::PushPull,
        sys_api::Speed::Low,
        sys_api::Pull::None,
    );

    // Populate packrat with our mac address and identity. Doing this now lets
    // the netstack wake up and start being useful while we're mucking around
    // with GPIOs below.
    let packrat = Packrat::from(PACKRAT.get_task_id());
    read_vpd_and_load_packrat(&packrat, I2C.get_task_id());

    let jefe = Jefe::from(JEFE.get_task_id());
    jefe.set_state(PowerState::A2 as u32);

    // Delay to allow things to settle, in case we were hot-plugged.
    hl::sleep_for(STARTUP_SETTLE_MS);

    // Check the status of the PSU ON nets, which indicate the current commanded
    // status of the PSUs. We can use this information to seed our state
    // machines, and also to make sure we don't glitch the PSUs.
    //
    // Note that, on power-on reset, these pins default to being configured
    // Analog, preventing us from reading their state. This is okay. In Analog
    // mode, an STM32 pin is defined as reading as 0, so we will see any such
    // pins as "PSU is ON" and switch the pin to input below. It is only if this
    // task has _restarted_ that we'll find pins set to input seeing 0, or
    // output seeing 1.
    let initial_psu_enabled: [bool; PSU_COUNT] = {
        let bits = sys.gpio_read(ALL_PSU_ENABLE_L_PINS);
        // ON signals are active-low, so we check for the _absence_ of the bit:
        core::array::from_fn(|i| bits & (1 << PSU_ENABLE_L_PINS[i]) == 0)
    };

    // Since we mostly just toggle the PSU ON nets between input and output, we
    // don't actually want to configure them at all at this stage. They're
    // either set input (in which case the PSU is being asked to be "on") or
    // output (in which case we're holding the PSU off, and will start a fault
    // resume sequence shortly).
    //
    // Ensure that the subset of pins that are currently undriven (which is to
    // say, ENABLE line low, PSU on) are set as inputs. Leave any pins observed
    // as 1 configured as they are. (See the rationale for this above on the
    // initial read.)
    sys.gpio_configure_input(
        {
            let mut inpins = PinSet {
                port: PSU_ENABLE_L_PORT,
                pin_mask: 0,
            };
            for (on, pinno) in
                initial_psu_enabled.into_iter().zip(PSU_ENABLE_L_PINS)
            {
                if on {
                    inpins = inpins.and_pin(pinno);
                }
            }
            // This set might be empty. That's ok; sys tolerates this.
            inpins
        },
        Pull::None,
    );

    // While we are not going to explicitly configure any pins as outputs at
    // this stage, for toggling the pins between input and output to work
    // properly, we need to pre-arrange for the pins to be high once they _are_
    // set to output. We do that here. If the pin is input, this has no effect;
    // if it's output, this should be a no-op because our previous incarnation
    // will have done this before setting it to output.
    sys.gpio_set_to(ALL_PSU_ENABLE_L_PINS, true);

    // Now, configure the presence/OK detect nets. We want these to be inputs;
    // at power-on reset they're analog. Switching pins between those two modes
    // cannot glitch, and nobody would be listening if it did.
    sys.gpio_configure_input(ALL_PSU_PWR_OK_PINS, Pull::None);
    sys.gpio_configure_input(ALL_PSU_PRESENT_L_PINS, Pull::None);

    // Collect all of the pin-change notifications we want into a mask word.
    // We'll use this each time we want to listen for pins.
    let all_pin_notifications = {
        let mut bits = 0;
        for mask in PSU_PWR_OK_NOTIF {
            bits |= mask;
        }
        bits
    };

    // Turn on pin change notifications on all of our input nets.
    sys.gpio_irq_configure(all_pin_notifications, Edge::Both);

    // Set up our state machines for each PSU. The initial state is always set
    // as present, and only the fault state is set based on our sensing of the
    // ON lines. This lets the normal logic used for handling absence and faults
    // in the loop below also handle the startup case.
    let start_time = sys_get_timer().now;
    let psu_states: [PsuState; PSU_COUNT] = core::array::from_fn(|i| {
        PsuState::Present(if initial_psu_enabled[i] {
            ringbuf_entry!((start_time, Trace::FoundEnabled { psu: i as u8 }));
            PresentState::On
        } else {
            // PSU was forced off by our previous incarnation. Schedule it to
            // turn back on in the future if things clear up.
            ringbuf_entry!((
                start_time,
                Trace::FoundAlreadyDisabled { psu: i as u8 }
            ));
            PresentState::Faulted {
                turn_on_deadline: start_time.saturating_add(FAULT_OFF_MS),
            }
        })
    });
    let mut psus = psu_states.map(|state| Psu { state });

    // Turn the chassis LED on to indicate that we're alive.
    sys.gpio_set(STATUS_LED);
    // TODO: if we wanted to kick jefe into a greater-than-A2 state, this'd be
    // where it happens.

    // Poll things.
    sys_set_timer(Some(start_time), notifications::TIMER_MASK);
    let sleep_notifications = all_pin_notifications | notifications::TIMER_MASK;
    loop {
        sys.gpio_irq_control(all_pin_notifications, IrqControl::Enable)
            .unwrap_lite();

        let present_l_bits = sys.gpio_read(ALL_PSU_PRESENT_L_PINS);
        let ok_bits = sys.gpio_read(ALL_PSU_PWR_OK_PINS);

        let now = sys_get_timer().now;
        for i in 0..PSU_COUNT {
            // Presence signals are active LOW.
            let present = if present_l_bits & (1 << PSU_PRESENT_L_PINS[i]) == 0
            {
                Present::Yes
            } else {
                Present::No
            };
            // PWR_OK signals are active HIGH.
            let ok = if ok_bits & (1 << PSU_PWR_OK_PINS[i]) != 0 {
                Status::Good
            } else {
                Status::NotGood
            };
            match psus[i].step(now, present, ok) {
                None => (),

                Some(ActionRequired::EnableMe) => {
                    ringbuf_entry!((now, Trace::Enabling { psu: i as u8 }));
                    // Enable the PSU by allowing `ENABLE_L` to float low, by no
                    // longer asserting high.
                    sys.gpio_configure_input(
                        PSU_ENABLE_L_PORT.pin(PSU_ENABLE_L_PINS[i]),
                        Pull::None,
                    );
                }
                Some(ActionRequired::DisableMe { attempt_snapshot }) => {
                    if attempt_snapshot {
                        // TODO snapshot goes here
                    }
                    ringbuf_entry!((
                        now,
                        Trace::Disabling {
                            psu: i as u8,
                            present: attempt_snapshot,
                        }
                    ));

                    // Pull `ENABLE_L` high to disable the PSU.
                    sys.gpio_configure_output(
                        PSU_ENABLE_L_PORT.pin(PSU_ENABLE_L_PINS[i]),
                        OutputType::PushPull,
                        Speed::Low,
                        Pull::None,
                    );
                }
            }
        }

        // Wait for a pin change or timer.
        let n = sys_recv_notification(sleep_notifications);
        if n & notifications::TIMER_MASK != 0 {
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
    state: PsuState,
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
            (_, Present::No, _) => {
                self.state = PsuState::NotPresent;
                Some(ActionRequired::DisableMe {
                    attempt_snapshot: false,
                })
            }

            // In a not-present situation we have to ignore the OK line entirely
            // and only watch for the presence line to indicate the PSU has
            // appeared.
            (PsuState::NotPresent, Present::Yes, _) => {
                let settle_deadline = now.wrapping_add(INSERT_DEBOUNCE_MS);
                self.state = PsuState::Present(PresentState::NewlyInserted {
                    settle_deadline,
                });
                // No external action required until our timer elapses.
                None
            }

            (
                PsuState::Present(PresentState::NewlyInserted {
                    settle_deadline,
                }),
                _,
                _,
            ) => {
                if settle_deadline <= now {
                    // The PSU is still present (since the Present::No case above
                    // didn't fire) and our deadline has elapsed. Let's treat this
                    // as valid!
                    self.state = PsuState::Present(PresentState::On);
                    Some(ActionRequired::EnableMe)
                } else {
                    // Remain in this state.
                    None
                }
            }

            // yay!
            (PsuState::Present(PresentState::On), _, Status::Good) => None,

            (PsuState::Present(PresentState::On), _, Status::NotGood) => {
                // The PSU appears to have pulled the OK signal into the "not
                // OK" state to indicate an internal fault!

                let turn_on_deadline = now.wrapping_add(FAULT_OFF_MS);
                self.state = PsuState::Present(PresentState::Faulted {
                    turn_on_deadline,
                });
                Some(ActionRequired::DisableMe {
                    attempt_snapshot: true,
                })
            }

            (
                PsuState::Present(PresentState::Faulted { turn_on_deadline }),
                _,
                _,
            ) => {
                if turn_on_deadline <= now {
                    // We turn the PSU back on _without regard_ to the OK signal
                    // state, because the PSU won't assert OK when it's off! We
                    // learned this the hard way. See #1800.
                    self.state = PsuState::Present(PresentState::On);
                    Some(ActionRequired::EnableMe)
                } else {
                    // Remain in this state.
                    None
                }
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
