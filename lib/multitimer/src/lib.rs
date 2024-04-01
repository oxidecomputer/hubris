// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A timer multiplexer.
//!
//! `Multitimer` lets you wrap a single underlying event timer and treat it as
//! multiple independent event timers. The independent event timers correspond
//! to variants of an enum type, to make it easy to tell them apart.
//!
//! The expected usage model is:
//!
//! - Create an `enum` type naming your timers, and derive the `Enum` trait
//!   (from the `enum_map` crate) for it.
//!
//! - Create a `Multitimer<YourEnumType>`.
//!
//! - Use its API to configure your timers to your heart's content.
//!
//! - When notifications arrive from the underlying timer, feed them into
//!   `Multitimer::handle_notification`.
//!
//! - When you're ready to process timer events (which may or may not be
//!   immediately after the notification), call `Multitimer::iter_fired`.
//!
//! **Note:** the `Multitimer` assumes that it has sole control of the
//! underlying timer. If you create two `Multitimer`s using the same underlying
//! timer, they will fight and the results will be unpleasant. API like
//! `sleep_until`/`sleep_for` that saves and restores timer settings _can_ be
//! used alongside `Multitimer`.

#![cfg_attr(target_os = "none", no_std)]

use enum_map::{EnumArray, EnumMap};

// Import the actual syscalls if we're targeting actual Hubris; otherwise we use
// some stub functions defined below.
#[cfg(target_os = "none")]
use userlib::{sys_get_timer, sys_set_timer};

pub struct Multitimer<E: EnumArray<Timer>> {
    notification_bit: u8,
    current_setting: Option<u64>,
    timers: EnumMap<E, Timer>,
}

impl<E: EnumArray<Timer> + Copy> Multitimer<E> {
    pub fn new(notification_bit: u8) -> Self {
        Self {
            notification_bit,
            current_setting: None,
            timers: EnumMap::default(),
        }
    }

    // Any time we call `sys_set_timer` we also need to record that setting in
    // `self.current_setting`; all timer sets should go through this helper.
    fn set_system_timer(&mut self, deadline: Option<u64>) {
        sys_set_timer(deadline, 1 << self.notification_bit);
        self.current_setting = deadline;
    }

    /// Sets the timer chosen by `which` to go off at time `deadline`, with
    /// optional auto-repeat behavior. This replaces any prior setting for the
    /// timer and enables it.
    ///
    /// This operation may cause a syscall: if `deadline` is sooner than all the
    /// other deadlines being managed by this multitimer, we will call into the
    /// OS to move our timer forward.
    pub fn set_timer(
        &mut self,
        which: E,
        deadline: u64,
        repeat: Option<Repeat>,
    ) {
        // If the timer has previously fired without us noticing it, preserve
        // that across set.
        let fired_but_not_observed = self.timers[which].fired_but_not_observed;
        self.timers[which] = Timer {
            deadline: Some((deadline, repeat)),
            fired_but_not_observed,
        };

        match self.current_setting {
            Some(current) if deadline >= current => (),
            _ => {
                self.set_system_timer(Some(deadline));
            }
        }
    }

    pub fn get_timer(&self, which: E) -> Option<(u64, Option<Repeat>)> {
        self.timers[which].deadline
    }

    pub fn clear_timer(&mut self, which: E) -> bool {
        let former_setting = self.timers[which].deadline.take();

        // If the timer was previously engaged, we may need to cancel our timer
        // with the OS.
        if let Some((former_dl, _)) = former_setting {
            // See if this timer could be responsible for our OS setting.
            if self.current_setting == Some(former_dl) {
                // Time to change it then.
                let new_earliest = self
                    .timers
                    .values()
                    .filter_map(|timer| timer.deadline)
                    .map(|(dl, _repeat)| dl)
                    .min();
                self.set_system_timer(new_earliest);
            }
        }

        former_setting.is_some()
    }

    /// Process a notification that may indicate that some timers are ready.
    ///
    /// This will mark the timers as having fired; you can read out the fired
    /// timers (destructively) using `iter_fired()`.
    pub fn handle_notification(&mut self, notification: u32) {
        if notification & 1 << self.notification_bit == 0 {
            // This isn't relevant to us.
            return;
        }

        let t = sys_get_timer().now;

        // As a premature optimization, we'll keep track of the new earliest
        // deadline after the timers have fired and only make one pass over the
        // table.
        let mut new_earliest = None;

        for timer in self.timers.values_mut() {
            // If the timer is on,
            if let Some((d, r)) = timer.deadline {
                // And the deadline has elapsed,
                if d <= t {
                    // Apply the repeat setting or disable the timer.
                    if let Some(kind) = r {
                        let next = match kind {
                            Repeat::AfterWake(period) => {
                                t.saturating_add(period)
                            }
                            Repeat::AfterDeadline(period) => {
                                d.saturating_add(period)
                            }
                        };
                        timer.deadline = Some((next, r));
                    } else {
                        timer.deadline = None;
                    }
                    // Record that it fired.
                    timer.fired_but_not_observed = true;
                }
                // If the timer is _still_ on,
                if let Some((new_d, _)) = timer.deadline {
                    new_earliest = Some(if let Some(earliest) = new_earliest {
                        new_d.min(earliest)
                    } else {
                        new_d
                    });
                }
            }
        }

        self.set_system_timer(new_earliest);
    }

    /// Checks all timer states unconditionally. This can be useful if you're
    /// running a fast loop without waiting for notifications.
    pub fn poll_now(&mut self) {
        self.handle_notification(1 << self.notification_bit);
    }

    /// Returns an iterator over all timers that have fired since the last time
    /// they were observed through this function. A timer may have fired more
    /// than once; that information is lost.
    ///
    /// Timers that have fired will appear in the order given by their `Enum`
    /// implementation, which in practice means declaration order.
    ///
    /// If you drop the iterator before it's exhausted, any timers you didn't
    /// observe will appear next time you call this.
    pub fn iter_fired(&mut self) -> impl Iterator<Item = E> + '_ {
        self.timers.iter_mut().filter_map(move |(e, timer)| {
            if core::mem::replace(&mut timer.fired_but_not_observed, false) {
                Some(e)
            } else {
                None
            }
        })
    }
}

#[derive(Copy, Clone, Default)]
pub struct Timer {
    deadline: Option<(u64, Option<Repeat>)>,
    fired_but_not_observed: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Repeat {
    AfterWake(u64),
    AfterDeadline(u64),
}

// Syscall fakes for testing!

#[cfg(not(target_os = "none"))]
mod fakes {
    use core::cell::Cell;

    thread_local! {
        pub static CURRENT_TIME: Cell<u64> = Cell::new(0);
        pub static TIMER_SETTING: Cell<(Option<u64>, u32)> = Cell::default();
    }

    pub fn sys_set_timer(deadline: Option<u64>, not: u32) {
        TIMER_SETTING.with(|s| s.set((deadline, not)));
    }

    pub fn sys_get_timer() -> TimerState {
        let now = CURRENT_TIME.with(|t| t.get());
        let (deadline, on_dl) = TIMER_SETTING.with(|s| s.get());
        TimerState {
            now,
            deadline,
            on_dl,
        }
    }

    #[allow(dead_code)]
    pub struct TimerState {
        pub now: u64,
        pub deadline: Option<u64>,
        pub on_dl: u32,
    }
}
#[cfg(not(target_os = "none"))]
use self::fakes::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn change_time(time: u64) {
        CURRENT_TIME.with(|t| t.set(time));
    }

    use enum_map::Enum;

    #[derive(Copy, Clone, Debug, Eq, PartialEq, Enum)]
    enum Timers {
        A,
        B,
    }

    fn make_uut(bit: u8) -> Multitimer<Timers> {
        Multitimer {
            notification_bit: bit,
            current_setting: None,
            timers: EnumMap::from_array([Timer::default(); Timers::LENGTH]),
        }
    }

    #[test]
    fn nothing_fired() {
        let mut uut = make_uut(0);

        assert!(uut.iter_fired().next().is_none());
    }

    #[test]
    fn setting_timer_propagates() {
        let mut uut = make_uut(0);

        uut.set_timer(Timers::A, 1234, None);

        let s = sys_get_timer();
        assert_eq!(s.deadline, Some(1234));
        assert_eq!(s.on_dl, 1 << 0);
    }

    #[test]
    fn earlier_timer_overrides() {
        let mut uut = make_uut(0);

        uut.set_timer(Timers::A, 1234, None);
        uut.set_timer(Timers::B, 12, None);

        let s = sys_get_timer();
        assert_eq!(s.deadline, Some(12));
        assert_eq!(s.on_dl, 1 << 0);
    }

    #[test]
    fn clear_timer_resets_undertimer() {
        let mut uut = make_uut(0);

        uut.set_timer(Timers::A, 1234, None);
        uut.set_timer(Timers::B, 12, None);
        uut.clear_timer(Timers::B);

        let s = sys_get_timer();
        assert_eq!(s.deadline, Some(1234));
        assert_eq!(s.on_dl, 1 << 0);
    }

    #[test]
    fn clear_all_timers_disables() {
        let mut uut = make_uut(0);

        uut.set_timer(Timers::A, 1234, None);
        uut.set_timer(Timers::B, 12, None);
        uut.clear_timer(Timers::A);
        uut.clear_timer(Timers::B);

        assert_eq!(sys_get_timer().deadline, None);
    }

    #[test]
    fn basic_firing_behavior() {
        change_time(0);
        let mut uut = make_uut(0);

        uut.set_timer(Timers::A, 1234, None);
        uut.set_timer(Timers::B, 12, None);

        // The time hasn't yet reached our earliest deadline, so notifications
        // should be no-ops.
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().next(), None);

        // Advance partway.
        change_time(11);
        // Still nothing.
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().next(), None);

        // Advance past one timer.
        change_time(100);
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().collect::<Vec<_>>(), [Timers::B]);

        // Advance past the other.
        change_time(10_000);
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().collect::<Vec<_>>(), [Timers::A]);

        // Neither timer resets, so, we shouldn't see further events.
        change_time(10_000_000);
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().next(), None);
    }

    #[test]
    fn repeat() {
        change_time(0);
        let mut uut = make_uut(0);

        // Timer A will go off at 1234, 2234, 3234, ...
        uut.set_timer(Timers::A, 1234, Some(Repeat::AfterDeadline(1000)));
        // Timer B will go off at 12, and then every 1000 ticks _after the
        // firing was observed._
        uut.set_timer(Timers::B, 12, Some(Repeat::AfterWake(2000)));

        // The time hasn't yet reached our earliest deadline, so notifications
        // should be no-ops.
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().next(), None);

        // Advance partway.
        change_time(11);
        // Still nothing.
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().next(), None);

        // Advance past timer B. We're going to advance _well past_ to test its
        // AfterWake behavior.
        change_time(100);
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().collect::<Vec<_>>(), [Timers::B]);

        // Timer B should now be set 2000 ticks _from now_ and still repeating.
        assert_eq!(
            uut.get_timer(Timers::B),
            Some((100 + 2000, Some(Repeat::AfterWake(2000)))),
        );

        // Advance past the other but before timer B recurs
        change_time(1300);
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().collect::<Vec<_>>(), [Timers::A]);

        // Timer A uses AfterDeadline behavior so it should now be set for
        // precisely 2234, and _not_ 2300.
        assert_eq!(
            uut.get_timer(Timers::A),
            Some((2234, Some(Repeat::AfterDeadline(1000)))),
        );

        // Trigger both timers again.
        change_time(2234);
        uut.handle_notification(!0);
        assert_eq!(
            uut.iter_fired().collect::<Vec<_>>(),
            [Timers::A, Timers::B],
        );
    }

    #[test]
    fn clear_and_reset() {
        change_time(0);
        let mut uut = make_uut(0);

        // Set A to go off at 10 and B to go off at 20
        uut.set_timer(Timers::A, 10, None);
        uut.set_timer(Timers::B, 20, None);

        // System timer should be set to 10, the earliest deadline.
        assert_eq!(sys_get_timer().deadline, Some(10));

        // Clear A, then reset it for 15.
        uut.clear_timer(Timers::A);
        uut.set_timer(Timers::A, 15, None);

        // System timer should be set to 15, the new earliest deadline.
        assert_eq!(sys_get_timer().deadline, Some(15));

        // Advance to T=16; A should fire.
        change_time(16);
        uut.handle_notification(!0);
        assert_eq!(uut.iter_fired().collect::<Vec<_>>(), [Timers::A]);

        // Set A to go off at 18, and check that the system timer is set
        // accordingly.
        uut.set_timer(Timers::A, 18, None);
        assert_eq!(sys_get_timer().deadline, Some(18));
    }
}
