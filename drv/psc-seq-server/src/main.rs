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

use drv_i2c_api::I2cDevice;
use drv_i2c_devices::mwocp68::{self, Mwocp68};
use drv_packrat_vpd_loader::{read_vpd_and_load_packrat, Packrat};
use drv_psc_seq_api::PowerState;
use drv_stm32xx_sys_api as sys_api;
use sys_api::{Edge, IrqControl, OutputType, PinSet, Pull, Speed};
use task_jefe_api::Jefe;
use userlib::*;

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
        serial: Option<[u8; 12]>,
    },
    /// Emitted at task startup when we find that a power supply appears to have
    /// been disabled.
    FoundAlreadyDisabled {
        now: u64,
        #[count(children)]
        psu: Slot,
        serial: Option<[u8; 12]>,
    },
    /// Emitted when a previously not present PSU's presence pin is asserted.
    Inserted {
        now: u64,
        #[count(children)]
        psu: Slot,
        serial: Option<[u8; 12]>,
    },
    /// Emitted when a previously present PSU's presence pin is deasserted.
    Removed {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Emitted when we decide a power supply should be on.
    Enabling {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    /// Emitted when we decide a power supply should be off; the `present` flag
    /// means the PSU is being turned off despite being present (`true`) or is
    /// being disabled because it's been removed (`false`).
    Disabling {
        now: u64,
        #[count(children)]
        psu: Slot,
        present: bool,
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
/// whenever a rectifier's `PWR_OK` pin changes state. Since exactly one of each
/// register entry is recorded for every `Faulted` and `FaultCleared` entry, we
/// don't really need to spend extra bytes on counting them, so they are marked
/// as `count(skip)`.
#[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    Faulted {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    FaultCleared {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    StillInFault {
        now: u64,
        #[count(children)]
        psu: Slot,
    },
    #[count(skip)]
    StatusWord {
        now: u64,
        psu: Slot,
        status_word: Result<u16, mwocp68::Error>,
    },
    #[count(skip)]
    StatusIout {
        now: u64,
        psu: Slot,
        status_iout: Result<u8, mwocp68::Error>,
    },
    #[count(skip)]
    StatusVout {
        now: u64,
        psu: Slot,
        status_vout: Result<u8, mwocp68::Error>,
    },
    #[count(skip)]
    StatusInput {
        now: u64,
        psu: Slot,
        status_input: Result<u8, mwocp68::Error>,
    },
    #[count(skip)]
    StatusCml {
        now: u64,
        psu: Slot,
        status_cml: Result<u8, mwocp68::Error>,
    },
    #[count(skip)]
    StatusTemperature {
        now: u64,
        psu: Slot,
        status_temperature: Result<u8, mwocp68::Error>,
    },
    #[count(skip)]
    StatusMfrSpecific {
        now: u64,
        psu: Slot,
        status_mfr_specific: Result<u8, mwocp68::Error>,
    },
    I2cError {
        now: u64,
        #[count(children)]
        psu: Slot,
        err: mwocp68::Error,
    },
    EreportSent {
        now: u64,
        #[count(children)]
        psu: Slot,
        class: ereport::Class,
        len: usize,
    },
    EreportLost {
        now: u64,
        #[count(children)]
        psu: Slot,
        class: ereport::Class,
        len: usize,
        err: task_packrat_api::EreportWriteError,
    },
    EreportTooBig {
        now: u64,
        #[count(children)]
        psu: Slot,
        class: ereport::Class,
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

/// In order to get the PMBus devices by PSU index, we need a little lookup table guy.
const PSU_PMBUS_DEVS: [fn(TaskId) -> (I2cDevice, u8); PSU_COUNT] = [
    i2c_config::pmbus::v54_psu0,
    i2c_config::pmbus::v54_psu1,
    i2c_config::pmbus::v54_psu2,
    i2c_config::pmbus::v54_psu3,
    i2c_config::pmbus::v54_psu4,
    i2c_config::pmbus::v54_psu5,
];

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

/// How long to wait after a PSU is inserted, before we attempt to turn it on.
/// This does double-duty in both debouncing the presence line, and ensuring
/// that things are firmly mated before activating anything.
const INSERT_DEBOUNCE_MS: u64 = 1_000; // Current value is somewhat arbitrary.

/// How long after exiting a fault state before we require the PSU to start
/// asserting OK. Or, conversely, how long to ignore the OK output after
/// re-enabling a faulted PSU.
///
/// We have observed delays of up to 92 ms in practice. Leaving the PSU enabled
/// in a fault state shouldn't be destructive, so we've padded this to avoid
/// flapping.
const PROBATION_MS: u64 = 1000;

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
    /// The PSU is detected as present.
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

    /// We are allowing the ON signal to float active, as in the `On` state, but
    /// we're not convinced the PSU is okay. We enter this state when bringing a
    /// PSU out of an observed fault state, and it causes us to ignore its OK
    /// output for a brief period (the deadline parameter, initialized as
    /// current time plus `DEADLINE_MS`).
    ///
    /// We do this because PSUs have been observed, in practice, taking up to
    /// ~100ms to assert OK after being enabled.
    ///
    /// Once the deadline elapses, we'll transition to the `On` state and start
    /// requiring OK to be asserted.
    OnProbation { deadline: u64 },
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

    // Statically allocate a buffer for ereport CBOR encoding, so that it's not
    // on the stack.
    let ereport_buf = {
        use static_cell::ClaimOnceCell;

        static BUF: ClaimOnceCell<[u8; 256]> = ClaimOnceCell::new([0; 256]);
        BUF.claim()
    };

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

    // Set up our state machines for each PSU. We'll need to read the presence
    // pins to determine whether a PSU is present and if we should ask it for
    // its serial number.
    let present_l_bits = sys.gpio_read(ALL_PSU_PRESENT_L_PINS);
    let start_time = sys_get_timer().now;

    let mut psus: [Psu; PSU_COUNT] = core::array::from_fn(|i| {
        let dev = {
            let i2c = I2C.get_task_id();
            let make_dev = PSU_PMBUS_DEVS[i];
            let (dev, rail) = make_dev(i2c);
            Mwocp68::new(&dev, rail)
        };
        let slot = PSU_SLOTS[i];
        let mut fruid = PsuFruid::default();
        let state = if present_l_bits & (1 << PSU_PRESENT_L_PINS[i]) == 0 {
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
            let step = psus[i].step(now, present, ok);
            match step.action {
                None => (),

                Some(ActionRequired::EnableMe) => {
                    ringbuf_entry!(Event::Enabling {
                        now,
                        psu: PSU_SLOTS[i]
                    });
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
                    ringbuf_entry!(Event::Disabling {
                        now,
                        psu: PSU_SLOTS[i],
                        present: attempt_snapshot,
                    });

                    // Pull `ENABLE_L` high to disable the PSU.
                    sys.gpio_configure_output(
                        PSU_ENABLE_L_PORT.pin(PSU_ENABLE_L_PINS[i]),
                        OutputType::PushPull,
                        Speed::Low,
                        Pull::None,
                    );
                }
            }
            if let Some(ereport) = step.ereport {
                match packrat.serialize_ereport(&ereport, &mut ereport_buf[..])
                {
                    Ok(len) => ringbuf_entry!(
                        __TRACE,
                        Trace::EreportSent {
                            now,
                            psu: ereport.psu_slot,
                            len,
                            class: ereport.class,
                        }
                    ),
                    Err(task_packrat_api::EreportSerializeError::Packrat {
                        err,
                        len,
                    }) => ringbuf_entry!(
                        __TRACE,
                        Trace::EreportLost {
                            now,
                            psu: ereport.psu_slot,
                            len,
                            class: ereport.class,
                            err,
                        }
                    ),
                    Err(
                        task_packrat_api::EreportSerializeError::Serialize(_),
                    ) => ringbuf_entry!(
                        __TRACE,
                        Trace::EreportTooBig {
                            now,
                            psu: ereport.psu_slot,
                            class: ereport.class,
                        }
                    ),
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
    slot: Slot,
    state: PsuState,
    dev: Mwocp68,
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
    fn step(&mut self, now: u64, present: Present, pwr_ok: Status) -> Step {
        match (self.state, present, pwr_ok) {
            (PsuState::NotPresent, Present::No, _) => {
                // ignore the power good line, it is meaningless.
                Step::default()
            }

            // Regardless of our current state, if we observe the present line
            // low, treat the PSU as having been disconnected.
            //
            // Other than detecting removal, the main side effect of this
            // decision is that the "NewlyInserted" settle time starts after the
            // contacts are _done_ scraping, not when they start.
            (_, Present::No, _) => {
                ringbuf_entry!(Event::Removed {
                    now,
                    psu: self.slot
                });
                let ereport = ereport::Ereport {
                    class: ereport::Class::Removed,
                    version: 0,
                    dev_id: self.dev.i2c_device().component_id(),
                    psu_slot: self.slot,
                    fruid: self.fruid,
                    pmbus_status: None,
                };

                self.state = PsuState::NotPresent;
                // Clear the FRUID serial only *after* we have put it in the ereport.
                self.fruid = PsuFruid::default();

                Step {
                    action: Some(ActionRequired::DisableMe {
                        attempt_snapshot: false,
                    }),
                    ereport: Some(ereport),
                }
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
                Step::default()
            }

            (
                PsuState::Present(PresentState::NewlyInserted {
                    settle_deadline,
                }),
                _,
                _,
            ) => {
                // Hello, who are you?
                self.refresh_fruid(now);
                if settle_deadline <= now {
                    // The PSU is still present (since the Present::No case above
                    // didn't fire) and our deadline has elapsed. Let's treat this
                    // as valid!
                    self.state = PsuState::Present(PresentState::On {
                        was_faulted: false,
                    });
                    let ereport = ereport::Ereport {
                        class: ereport::Class::Inserted,
                        version: 0,
                        dev_id: self.dev.i2c_device().component_id(),
                        psu_slot: self.slot,
                        fruid: self.fruid,
                        pmbus_status: None,
                    };

                    Step {
                        action: Some(ActionRequired::EnableMe),
                        ereport: Some(ereport),
                    }
                } else {
                    // Remain in this state.
                    Step::default()
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
                let ereport = if was_faulted {
                    // Clear our tracking of the fault. If we fault again, treat
                    // that as a new fault.
                    self.state = PsuState::Present(PresentState::On {
                        was_faulted: false,
                    });
                    ringbuf_entry!(
                        __TRACE,
                        Trace::FaultCleared {
                            now,
                            psu: self.slot,
                        }
                    );
                    // Report that the fault has gone away.
                    Some(ereport::Ereport {
                        class: ereport::Class::FaultCleared,
                        version: 0,
                        dev_id: self.dev.i2c_device().component_id(),
                        psu_slot: self.slot,
                        fruid: self.fruid,
                        pmbus_status: Some(self.read_pmbus_status(now)),
                    })
                } else {
                    // If we did not just restart after a fault, do nothing.
                    None
                };
                Step {
                    action: None,
                    ereport,
                }
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
                let ereport = if !was_faulted {
                    ringbuf_entry!(
                        __TRACE,
                        Trace::Faulted {
                            now,
                            psu: self.slot,
                        }
                    );
                    Some(ereport::Ereport {
                        class: ereport::Class::Fault,
                        version: 0,
                        dev_id: self.dev.i2c_device().component_id(),
                        psu_slot: self.slot,
                        fruid: self.fruid,
                        pmbus_status: Some(self.read_pmbus_status(now)),
                    })
                } else {
                    ringbuf_entry!(
                        __TRACE,
                        Trace::StillInFault {
                            now,
                            psu: self.slot,
                        }
                    );
                    None
                };

                Step {
                    action: Some(ActionRequired::DisableMe {
                        attempt_snapshot: true,
                    }),
                    ereport,
                }
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
                    self.state = PsuState::Present(PresentState::OnProbation {
                        deadline: now.saturating_add(PROBATION_MS),
                    });
                    Step {
                        action: Some(ActionRequired::EnableMe),
                        ereport: None,
                    }
                } else {
                    // Remain in this state.
                    Step::default()
                }
            }
            (
                PsuState::Present(PresentState::OnProbation { deadline }),
                _,
                _,
            ) => {
                // Just in case we were previously unable to read any FRUID
                // values due to I2C weather, try to refresh them
                self.refresh_fruid(now);
                if deadline <= now {
                    // Take PSU out of probation state and start monitoring its
                    // OK line.
                    self.state = PsuState::Present(PresentState::On {
                        was_faulted: true,
                    });
                    Step::default()
                } else {
                    // Remain in this state.
                    Step::default()
                }
            }
        }
    }

    fn refresh_fruid(&mut self, now: u64) {
        self.fruid.refresh(&self.dev, self.slot, now);
    }

    fn read_pmbus_status(&mut self, now: u64) -> ereport::PmbusStatus {
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

        ereport::PmbusStatus {
            word: status_word.ok(),
            iout: status_iout.ok(),
            vout: status_vout.ok(),
            input: status_input.ok(),
            cml: status_cml.ok(),
            temp: status_temperature.ok(),
            mfr: status_mfr_specific.ok(),
        }
    }
}

#[derive(Default)]
struct Step {
    action: Option<ActionRequired>,
    ereport: Option<ereport::Ereport>,
}

#[derive(Copy, Clone, serde::Serialize, Default)]
struct PsuFruid {
    #[serde(serialize_with = "ereport::serialize_fixed_str")]
    mfr: Option<[u8; 9]>,
    #[serde(serialize_with = "ereport::serialize_fixed_str")]
    mpn: Option<[u8; 17]>,
    #[serde(serialize_with = "ereport::serialize_fixed_str")]
    serial: Option<[u8; 12]>,
    #[serde(serialize_with = "ereport::serialize_fixed_str")]
    fw_rev: Option<[u8; 4]>,
}

impl PsuFruid {
    fn refresh(&mut self, dev: &Mwocp68, psu: Slot, now: u64) {
        if self.mfr.is_none() {
            self.mfr =
                retry_i2c_txn(now, psu, || dev.mfr_id()).ok().map(|v| v.0);
        }

        if self.serial.is_none() {
            self.serial = retry_i2c_txn(now, psu, || dev.serial_number())
                .ok()
                .map(|v| v.0);
        }

        if self.mpn.is_none() {
            self.mpn = retry_i2c_txn(now, psu, || dev.model_number())
                .ok()
                .map(|v| v.0);
        }

        if self.fw_rev.is_none() {
            self.fw_rev = retry_i2c_txn(now, psu, || dev.firmware_revision())
                .ok()
                .map(|v| v.0);
        }
    }
}

fn retry_i2c_txn<T>(
    now: u64,
    psu: Slot,
    mut txn: impl FnMut() -> Result<T, mwocp68::Error>,
) -> Result<T, mwocp68::Error> {
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

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

mod ereport {
    use super::*;
    use serde::Serialize;

    #[derive(Copy, Clone, Eq, PartialEq, Serialize)]
    pub(super) enum Class {
        #[serde(rename = "psu.insert")]
        Inserted,
        #[serde(rename = "psu.remove")]
        Removed,
        #[serde(rename = "psu.fault")]
        Fault,
        #[serde(rename = "psu.fault_cleared")]
        FaultCleared,
    }

    #[derive(Copy, Clone, Serialize)]
    pub(super) struct Ereport {
        #[serde(rename = "k")]
        pub(super) class: Class,
        #[serde(rename = "v")]
        pub(super) version: u32,
        pub(super) dev_id: &'static str,
        #[serde(serialize_with = "serialize_psu_slot")]
        pub(super) psu_slot: Slot,
        pub(super) fruid: PsuFruid,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub(super) pmbus_status: Option<PmbusStatus>,
    }

    #[derive(Copy, Clone, Default, Serialize)]
    pub(super) struct PmbusStatus {
        pub(super) word: Option<u16>,
        pub(super) input: Option<u8>,
        pub(super) iout: Option<u8>,
        pub(super) vout: Option<u8>,
        pub(super) temp: Option<u8>,
        pub(super) cml: Option<u8>,
        pub(super) mfr: Option<u8>,
    }

    /// XXX(eliza): A "fixed length byte string" helper would be a nice thing to
    /// have...
    pub(super) fn serialize_fixed_str<const LEN: usize, S>(
        s: &Option<[u8; LEN]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        s.as_ref()
            .and_then(|s| str::from_utf8(&s[..]).ok())
            .serialize(serializer)
    }

    fn serialize_psu_slot<S>(
        slot: &Slot,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        (*slot as u8).serialize(serializer)
    }
}
