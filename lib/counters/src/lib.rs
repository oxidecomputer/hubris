// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! # Hubris Event Counters
//!
//! "Store hundreds of thousands of events in four bytes of RAM with this one
//! weird trick!"
//!
//! This crate provides the [`Count`] trait, which defines a countable event,
//! and the [`counters!`] macro, which declares a set of static counters

#![no_std]
pub use armv6m_atomic_hack;
#[cfg(feature = "derive")]
pub use counters_derive::Count;

///
/// A countable event.
///
/// This trait can (and generally should) be derived for an `enum`
/// type using the [`#[derive(Count)]`][drv] attribute.
///
/// [drv]: counters_derive::Count
pub trait Count {
    /// A type that counts occurances of this ringbuf entry.
    type Counters;

    /// Initializer for a new set of counters.
    ///
    /// The value of each counter in this constant should be 0.
    const NEW_COUNTERS: Self::Counters;

    /// Increment the counter for this event.
    fn count(&self, counters: &Self::Counters);
}

/// Declares a set of event counters.
///
/// `counters!(NAME, Type)` creates a set of counters named `NAME`, counting
/// occurences of `Type`. `Type` must implement the [`Count`] trait to be
/// counted.
///
/// The resulting counters will be static, so `NAME` should be uppercase. If no
/// name is provided, the static will be named `__COUNTERS`.
///
/// Once a set of counters is declared, events can be counted by calling the
/// [`Count::count`] method on the event type, with a reference to the counters
/// static.
#[macro_export]
macro_rules! counters {
    ($name:ident, $Type:ident) => {
        #[used]
        static $name: <$Type as $crate::Count>::Counters =
            <$Type as $crate::Count>::NEW_COUNTERS;
    };
    ($Type:ident) => {
        $crate::counters!(__COUNTERS, $Type);
    };
}

/// Count an event.
///
/// This is a very small wrapper around the [`Count::count`] method.
#[macro_export]
macro_rules! count {
    ($counters:expr, $event:expr) => {
        // Evaluate both counters and event, without letting them access each
        // other, by evaluating them in a tuple where each cannot
        // accidentally use the other's binding.
        let (e, ctrs) = ($event, &$counters);
        // Invoke these functions using slightly weird syntax to avoid
        // accidentally calling a _different_ routine called count.
        $crate::Count::count(&e, ctrs);
    };
    ($event:expr) => {
        $crate::count!(__COUNTERS, $event);
    };
}
