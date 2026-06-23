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
use core::sync::atomic::{AtomicU32, Ordering};
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
    ($name:ident, $Type:ty) => {
        static $name: <$Type as $crate::Count>::Counters =
            <$Type as $crate::Count>::NEW_COUNTERS;
    };
    ($Type:ty) => {
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

/// Counters for [`Result`]`<T, E>`s where `T` and `E` implement [`Count`].
#[allow(nonstandard_style)]
pub struct ResultCounters<T: Count, E: Count> {
    /// Counters for this [`Result`]'s [`Ok`] variant.
    pub Ok: T::Counters,
    /// Counters for this [`Result`]'s [`Err`] variant.
    pub Err: E::Counters,
}

/// Counters for [`Option`]`<T>`s where `T` implements [`Count`].
#[allow(nonstandard_style)]
pub struct OptionCounters<T: Count> {
    /// Counters for this [`Option`]'s [`Some`] variant.
    pub Some: T::Counters,
    /// The total number of [`None`]s recorded by this counter.
    pub None: AtomicU32,
}

impl<T: Count, E: Count> Count for Result<T, E> {
    type Counters = ResultCounters<T, E>;
    const NEW_COUNTERS: Self::Counters = ResultCounters {
        Ok: T::NEW_COUNTERS,
        Err: E::NEW_COUNTERS,
    };

    fn count(&self, counters: &Self::Counters) {
        match self {
            Ok(t) => t.count(&counters.Ok),
            Err(e) => e.count(&counters.Err),
        }
    }
}

impl<T: Count> Count for Option<T> {
    type Counters = OptionCounters<T>;

    #[allow(clippy::declare_interior_mutable_const)]
    const NEW_COUNTERS: Self::Counters = OptionCounters {
        Some: T::NEW_COUNTERS,
        None: AtomicU32::new(0),
    };

    fn count(&self, counters: &Self::Counters) {
        match self {
            Some(t) => t.count(&counters.Some),
            None => {
                armv6m_atomic_hack::AtomicU32Ext::fetch_add(
                    &counters.None,
                    1,
                    Ordering::Relaxed,
                );
            }
        }
    }
}

impl<T: Count, E: Count> Count for &'_ Result<T, E> {
    type Counters = ResultCounters<T, E>;
    const NEW_COUNTERS: Self::Counters = ResultCounters {
        Ok: T::NEW_COUNTERS,
        Err: E::NEW_COUNTERS,
    };

    fn count(&self, counters: &Self::Counters) {
        match self {
            Ok(t) => t.count(&counters.Ok),
            Err(e) => e.count(&counters.Err),
        }
    }
}

impl<T: Count> Count for &'_ Option<T> {
    type Counters = OptionCounters<T>;

    #[allow(clippy::declare_interior_mutable_const)]
    const NEW_COUNTERS: Self::Counters = OptionCounters {
        Some: T::NEW_COUNTERS,
        None: AtomicU32::new(0),
    };

    fn count(&self, counters: &Self::Counters) {
        match self {
            Some(t) => t.count(&counters.Some),
            None => {
                armv6m_atomic_hack::AtomicU32Ext::fetch_add(
                    &counters.None,
                    1,
                    Ordering::Relaxed,
                );
            }
        }
    }
}

impl Count for core::convert::Infallible {
    type Counters = ();
    const NEW_COUNTERS: Self::Counters = ();

    fn count(&self, _: &Self::Counters) {
        // `Infallible`s are not made. They should NEVER be made. We
        // will not count them. We will not help count them.
        match *self {}
    }
}

impl Count for () {
    type Counters = AtomicU32;
    #[allow(clippy::declare_interior_mutable_const)]
    const NEW_COUNTERS: Self::Counters = AtomicU32::new(0);

    fn count(&self, counters: &Self::Counters) {
        armv6m_atomic_hack::AtomicU32Ext::fetch_add(
            counters,
            1,
            Ordering::Relaxed,
        );
    }
}

/// Counters for [`bool`]s.
///
/// This allows placing the `#[count(children)]`
/// attribute on `bool` fields in `enum` variants.
pub struct BoolCounts {
    /// The total number of times these counters have recorded a `true` value.
    pub r#true: AtomicU32,
    /// The total number of times these counters have recorded a `false`
    /// value.
    pub r#false: AtomicU32,
}

impl Count for bool {
    type Counters = BoolCounts;

    #[allow(clippy::declare_interior_mutable_const)]
    const NEW_COUNTERS: Self::Counters = BoolCounts {
        r#true: AtomicU32::new(0),
        r#false: AtomicU32::new(0),
    };

    fn count(&self, counters: &Self::Counters) {
        let ctr = match self {
            true => &counters.r#true,
            false => &counters.r#false,
        };

        armv6m_atomic_hack::AtomicU32Ext::fetch_add(ctr, 1, Ordering::Relaxed);
    }
}
