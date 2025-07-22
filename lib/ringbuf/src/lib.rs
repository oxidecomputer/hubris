// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Ring buffer for debugging Hubris tasks and drivers
//!
//! This contains an implementation for a static ring buffer designed to be used
//! to instrument arbitrary contexts.  While there is nothing to prevent these
//! ring buffers from being left in production code, the design center is
//! primarily around debugging in development: the ring buffers themselves can
//! be processed either with Humility (which has built-in support via the
//! `humility ringbuf` command) or via GDB.
//!
//! ## Constraints
//!
//! The main constraint for a ring buffer is that the type in the ring buffer
//! must implement [`Copy`]. If [de-duplication](#entry-de-duplication) is
//! enabled, the entry type must also implement [`PartialEq`]. When using the
//! [`counted_ringbuf!`] macro to [count ring buffer
//! entries](#counted-ring-buffers), the entry type must implement the
//! [`counters::Count`] trait.
//!
//! If you use the variants of the `ringbuf!` macro that leave the name of the
//! data structure implicit, you can only have one per module. (You can lift
//! this constraint by providing a name.)
//!
//! ## Creating a ring buffer
//!
//! Ring buffers are instantiated with the [`ringbuf!`] macro, to which one
//! must provide the type of per-entry payload, the number of entries, and a
//! static initializer.  For example, to define a 16-entry ring buffer with
//! each entry containing a [`core::u32`].
//!
//! ```
//! ringbuf!(u32, 16, 0);
//! ```
//!
//! Ring buffer entries are generated with [`ringbuf_entry!`] specifying a
//! payload of the appropriate type, e.g.:
//!
//! ```
//! ringbuf_entry!(isr.bits());
//! ```
//!
//! You can also provide a name for the ring buffer, to distinguish between them
//! if you have more than one:
//!
//! ```
//! ringbuf!(MY_RINGBUF, u32, 16, 0);
//!
//! // ...
//!
//! ringbuf_entry!(MY_RINGBUF, isr.bits());
//! ```
//!
//! Payloads can obviously be more sophisticated; for example, here's a payload
//! that takes a floating point value and an optional register:
//!
//! ```
//! ringbuf!((f32, Option<Register>), 128, (0.0, None));
//! ```
//!
//! For which one might add an entry with (say):
//!
//! ```
//! ringbuf_entry!((temp, Some(Register::TempMSB)));
//! ```
//!
//! ### Counted ring buffers
//!
//! One limitation of ring buffers for recording diagnostic data is that, when a
//! very large number of entries have been recorded, historical data may not be
//! available, as the earliest entries may have have "fallen off" the end of the
//! ring buffer and been overwritten. Therefore, to preserve historical data in
//! ring buffers that record a large number of events, or where the size of the
//! ring buffer is small, this crate also provides a [`counted_ringbuf!] macro.
//!
//! The [`counted_ringbuf!`] macro is used to declare a ring buffer that records
//! a total count of each entry variant that has been recorded, in addition to
//! storing the last `N` entries. This way, some information about entries that
//! have been overwritten is still preserved, making it possible to determine
//! whether an entry variant has *ever* been recorded, even if it has been
//! overwritten. The data recorded by counters is less granular than the ringbuf
//! itself, as entry variants that hold data are collapsed into a single
//! counter, regardless of their field values, and the order in which entries
//! have occurred is not preserved by a counter.
//!
//! Entry variant counts are recorded even when this crate is compiled with the
//! "disabled" feature flag set. This allows targets which lack the memory for
//! an entire ring buffer to still record event counts. To disable counting ring
//! buffer entries, set the "counters-disabled" feature flag. This feature may
//! be set independently of the "disabled" feature: if both are set, neither
//! ring buffer entries nor counters will be recorded; if *only*
//! "counters-disabled" is set, the [`counted_ringbuf!`] macro will behave
//! identically to the [`ringbuf!`] macro, recording ringbuf entries but not
//! counters.
//!
//! To use the [`counted_ringbuf!`] macro, the entry type must be
//! an `enum`, and it must implement the [`counters::Count`] trait, which
//! defines a mechanism for counting occurences of each entry variant.
//! Typically, the [`Count`] trait is implemented using the `#[derive(Count)]`
//! attribute. The same [`ringbuf_entry!`] and [`ringbuf_entry_root!`] macros
//! can be used to record an entry in a counted ring buffer.
//!
//! For example:
//!
//! ```
//! // Declare an enum type and derive the `Count` trait for it:
//! #[derive(Copy, Clone, Debug, PartialEq, Eq, counters::Count)]
//! pub enum MyEvent {
//!     NothingHappened,
//!     SomethingHappened,
//!     SomethingElseHappened(u32),
//!     // ...
//! }
//!
//! // Declare a counted ring buffer of `MyEvent` entries:
//! counted_ringbuf!(MyEvent, 16, MyEvent::NothingHappened);
//!
//! // Record an entry in the counted ring buffer, incrementing the counter
//! // for the `MyEvent::SomethingHappened` variant:
//! ringbuf_entry!(MyEvent::SomethingHappened);
//!
//! // Record an entry variant with data. Note that both of these entries will
//! // increment the *same* counter (`SomethingElseHappened`), despite having
//! // different values:
//! ringbuf_entry!(MyEvent::SomethingElseHappened(42));
//! ringbuf_entry!(MyEvent::SomethingElseHappened(666));
//! ```
//!
//! ### Entry de-duplication
//!
//! By default, when the same value is recorded in a ring buffer multiple times
//! in a row, the subsequent entries are recorded by incrementing a counter
//! stored in the initial entry, rather than by adding new entries to the
//! ringbuf. This de-duplication prevents the ring buffer from filling up with a
//! large number of duplicate entries, allowing the earlier history to be
//! recorded.
//!
//! However, this de-duplication requires the entry type to implement the
//! [`PartialEq`] trait, and performs a comparison with the previous entry
//! whenever an entry is recorded. The [`PartialEq`] implementation and
//! comparisons can have a meaningful impact on binary size, especially when the
//! entry type is complex. Therefore, code which does not record a large number
//! of duplicate entries, or which does not care about de-duplicating them, can
//! disable de-duplication by adding the `no_dedup` argument at the end of the
//! [`ringbuf!`] or [`counted_ringbuf!`] macro. For example:
//!
//! ```
//! ringbuf!(u32, 16, 0, no_dedup);
//! ```
//!
//! Or, with [`counted_ringbuf!`]:
//!
//! ````
//! #[derive(Copy, Clone, Debug, Eq, counters::Count)]
//! pub enum MyEvent {
//!     NothingHappened,
//!     SomethingHappened,
//!     SomethingElseHappened(u32),
//!     // ...
//! }
//! counted_ringbuf!(MyEvent, 16, MyEvent::NothingHappened, no_dedup);
//! ```
//!
//! ## Inspecting a ring buffer via Humility
//!
//! Humility has built-in support for dumping a ring buffer, and will (by
//! default) look for and dump any ring buffer declared with [`ringbuf!`], e.g.:
//!
//! ```console
//! $ cargo xtask humility app.toml ringbuf
//! humility: attached via ST-Link
//! humility: ring buffer MAX31790_RINGBUF in thermal:
//! ADDR        NDX LINE  GEN    COUNT PAYLOAD
//! 0x20007774    1  242   12        1 (Some(Tach5CountMSB), Ok([ 0xff, 0xe0 ]))
//! 0x20007788    2  242   12        1 (Some(Tach6CountMSB), Ok([ 0xff, 0xe0 ]))
//! 0x2000779c    3  242   12        1 (Some(Tach1CountMSB), Ok([ 0x7d, 0xc0 ]))
//! 0x200077b0    4  242   12        1 (Some(Tach2CountMSB), Ok([ 0xff, 0xe0 ]))
//! 0x200077c4    5  242   12        1 (Some(Tach3CountMSB), Ok([ 0xff, 0xe0 ]))
//! 0x200077d8    6  242   12        1 (Some(Tach4CountMSB), Ok([ 0xff, 0xe0 ]))
//! 0x200077ec    7  242   12        1 (Some(Tach5CountMSB), Ok([ 0xff, 0xe0 ]))
//! 0x20007800    8  242   12        1 (Some(Tach6CountMSB), Ok([ 0xff, 0xe0 ]))
//! 0x20007814    9  242   12        1 (Some(Tach1CountMSB), Ok([ 0x7d, 0xe0 ]))
//! ...
//! ```
//!
//! You can also dump a particular ring buffer by giving its name.
//!
//! If for any reason a raw view is needed, one can also use `humility readvar`
//! and specify the corresponding `RINGBUF` variable.  (The name of the
//! variable is `RINGBUF` prefixed with the stem of the file that declared
//! it.)
//!
//! ## Inspecting a ring buffer via GDB
//!
//! Assuming symbols are loaded, one can use GDB's `print` command,
//! specifying the crate that contains the ring buffer and the appropraite
//! `RINGBUF` variable.  If the `thermal` task defines a ring buffer in
//! its main, it can be printed this way:
//!
//! ```console
//! (gdb) set print pretty on
//! (gdb) print task_thermal::RINGBUF
//!
//! $2 = task_thermal::Ringbuf<core::option::Option<drv_i2c_devices::max31790::Fan>> {
//!  last: core::option::Option<usize>::Some(3),
//!  buffer: [
//!    task_thermal::RingbufEntry<core::option::Option<drv_i2c_devices::max31790::Fan>> {
//!      line: 31,
//!      generation: 9,
//!      count: 1,
//!      payload: core::option::Option<drv_i2c_devices::max31790::Fan>::Some(drv_i2c_devices::max31790::Fan (
//!          3
//!        ))
//!    },...
//! ```
//!
//! To inspect a ring buffer that is in a dependency, the full crate will need
//! to be specified, e.g. to inspect a ring buffer that is used in the `max31790`
//! module of the `drv_i2c_devices` crate:
//!
//! ```console
//! (gdb) set print pretty on
//! (gdb) print drv_i2c_devices::max31790::MAX31790_RINGBUF
//! $3 = drv_i2c_devices::max31790::Ringbuf<(core::option::Option<drv_i2c_devices::max31790::Register>, core::result::Result<[u8; 2], drv_i2c_api::ResponseCode>)> {
//!  last: core::option::Option<usize>::Some(30),
//!  buffer: [
//!    drv_i2c_devices::max31790::RingbufEntry<(core::option::Option<drv_i2c_devices::max31790::Register>, core::result::Result<[u8; 2], drv_i2c_api::ResponseCode>)> {
//!      line: 242,
//!      generation: 79,
//!      count: 1,
//!      payload: (
//!        core::option::Option<drv_i2c_devices::max31790::Register>::Some(drv_i2c_devices::max31790::Register::Tach6CountMSB),
//!        core::result::Result<[u8; 2], drv_i2c_api::ResponseCode>::Err(0)
//!      )
//!    },...
//! ```
#![no_std]
#[cfg(feature = "counters")]
pub use counters::Count;
/// Re-export the bits we use from `static_cell` so that code generated by the
/// macros is guaranteed to be able to find them.
pub use static_cell::StaticCell;

#[cfg(feature = "disabled")]
#[macro_export]
macro_rules! ringbuf {
    ($name:ident, $t:ty, $n:expr, $init:expr, no_dedup) => {
        $crate::ringbuf!($name, $t, $n, $init)
    };
    ($name:ident, $t:ty, $n:expr, $init:expr) => {
        #[allow(dead_code)]
        const _: $t = $init;
        static $name: () = ();
    };
    ($t:ty, $n:expr, $init:expr, no_dedup) => {
        $crate::ringbuf!(__RINGBUF, $t, $n, $init);
    };
    ($t:ty, $n:expr, $init:expr) => {
        $crate::ringbuf!(__RINGBUF, $t, $n, $init);
    };
}

/// Declares a ringbuffer in the current module or context.
///
/// `ringbuf!(NAME, Type, N, expr)` makes a ringbuffer named `NAME`,
/// containing entries of type `Type`, with room for `N` such entries, all of
/// which are initialized to `expr`.
///
/// The resulting ringbuffer will be static, so `NAME` should be uppercase. If
/// you want your ringbuffer to be detected by Humility's automatic scan, its
/// name should end in `RINGBUF`.
///
/// The actual type of `name` will be `StaticCell<Ringbuf<T, N>>`.
///
/// To support the common case of having one quickly-installed ringbuffer per
/// module, if you omit the name, it will default to `__RINGBUF`.
#[cfg(not(feature = "disabled"))]
#[macro_export]
macro_rules! ringbuf {
    ($name:ident, $t:ty, $n:expr, $init:expr) => {
        static $name: $crate::StaticCell<$crate::Ringbuf<$t, u16, $n>> =
            $crate::StaticCell::new($crate::Ringbuf {
                last: None,
                buffer: [$crate::RingbufEntry {
                    line: 0,
                    generation: 0,
                    count: 0,
                    payload: $init,
                }; $n],
            });
    };
    ($name:ident, $t:ty, $n:expr, $init:expr, no_dedup) => {
        static $name: $crate::StaticCell<$crate::Ringbuf<$t, () $n>> =
            $crate::StaticCell::new($crate::Ringbuf {
                last: None,
                buffer: [$crate::RingbufEntry {
                    line: 0,
                    generation: 0,
                    count: (),
                    payload: $init,
                }; $n],
            });
    };
    ($t:ty, $n:expr, $init:expr, no_dedup) => {
        $crate::ringbuf!(__RINGBUF, $t, $n, $init, no_dedup);
    };
    ($t:ty, $n:expr, $init:expr) => {
        $crate::ringbuf!(__RINGBUF, $t, $n, $init);
    };
}

/// Declares a ringbuffer and set of event counts in the current module or
/// context.
///
/// `counted_ringbuf!(NAME, Type, N, expr)` makes a [`CountedRingbuf`] named `NAME`,
/// containing entries of type `Type`, with room for `N` such entries, all of
/// which are initialized to `expr`. `Type` must implement the [`Count`] trait,
/// which defines how to count occurences of each ringbuf entry variant. See
/// [the crate-level documentation](crate#counted-ring-buffers) for more
/// details on recording entry counts.
///
/// The resulting ringbuffer will be static, so `NAME` should be uppercase. If
/// you want your ringbuffer to be detected by Humility's automatic scan, its
/// name should end in `RINGBUF`.
///
/// To support the common case of having one quickly-installed ringbuffer per
/// module, if you omit the name, it will default to `__RINGBUF`.
///
#[cfg(all(
    not(feature = "disabled"),
    not(feature = "counters-disabled"),
    feature = "counters"
))]
#[macro_export]
macro_rules! counted_ringbuf {
    ($name:ident, $t:ident, $n:expr, $init:expr) => {
        static $name: $crate::CountedRingbuf<$t, u16, $n> =
            $crate::CountedRingbuf {
                ringbuf: $crate::StaticCell::new($crate::Ringbuf {
                    last: None,
                    buffer: [$crate::RingbufEntry {
                        line: 0,
                        generation: 0,
                        count: 0,
                        payload: $init,
                    }; $n],
                }),
                counters: <$t as $crate::Count>::NEW_COUNTERS,
            };
    };
    ($name:ident, $t:ident, $n:expr, $init:expr, no_dedup) => {
        static $name: $crate::CountedRingbuf<$t, (), $n> =
            $crate::CountedRingbuf {
                ringbuf: $crate::StaticCell::new($crate::Ringbuf {
                    last: None,
                    buffer: [$crate::RingbufEntry {
                        line: 0,
                        generation: 0,
                        count: (),
                        payload: $init,
                    }; $n],
                }),
                counters: <$t as $crate::Count>::NEW_COUNTERS,
            };
    };
    ($t:ident, $n:expr, $init:expr, no_dedup) => {
        $crate::counted_ringbuf!(__RINGBUF, $t, $n, $init, no_dedup);
    };
    ($t:ident, $n:expr, $init:expr) => {
        $crate::counted_ringbuf!(__RINGBUF, $t, $n, $init);
    };
}

#[cfg(all(
    feature = "counters",
    not(feature = "counters-disabled"),
    feature = "disabled"
))]
#[macro_export]
macro_rules! counted_ringbuf {
    ($name:ident, $t:ident, $n:expr, $init:expr, no_dedup) => {
        #[used]
        static $name: $crate::CountedRingbuf<$t, (), $n> =
            $crate::CountedRingbuf {
                counters: <$t as $crate::Count>::NEW_COUNTERS,
                _c: core::marker::PhantomData,
            };
    };
    ($name:ident, $t:ident, $n:expr, $init:expr) => {
        #[used]
        static $name: $crate::CountedRingbuf<$t, u16, $n> =
            $crate::CountedRingbuf {
                counters: <$t as $crate::Count>::NEW_COUNTERS,
                _c: core::marker::PhantomData,
            };
    };
    ($t:ident, $n:expr, $init:expr, no_dedup) => {
        $crate::counted_ringbuf!(__RINGBUF, $t, $n, $init, no_dedup);
    };
    ($t:ident, $n:expr, $init:expr) => {
        $crate::counted_ringbuf!(__RINGBUF, $t, $n, $init);
    };
}

#[cfg(all(
    feature = "counters",
    feature = "counters-disabled",
    not(feature = "disabled")
))]
#[macro_export]
macro_rules! counted_ringbuf {
    ($name:ident, $t:ident, $n:expr, $init:expr, no_dedup) => {
        $crate::ringbuf!($name, $t, $n, $init, no_dedup)
    };
    ($name:ident, $t:ident, $n:expr, $init:expr) => {
        $crate::ringbuf!($name, $t, $n, $init)
    };
    ($t:ident, $n:expr, $init:expr, no_dedup) => {
        $crate::ringbuf!(__RINGBUF, $t, $n, $init, no_dedup);
    };
    ($t:ident, $n:expr, $init:expr) => {
        $crate::ringbuf!(__RINGBUF, $t, $n, $init);
    };
}

#[cfg(all(
    feature = "counters",
    feature = "counters-disabled",
    feature = "disabled"
))]
#[macro_export]
macro_rules! counted_ringbuf {
    ($name:ident, $t:ident, $n:expr, $init:expr, no_dedup) => {
        $crate::counted_ringbuf!(%name, $t, $n, $init)
    };
    ($name:ident, $t:ident, $n:expr, $init:expr) => {
        #[allow(dead_code)]
        const _: $t = $init;
        static $name: () = ();
    };
    ($t:ident, $n:expr, $init:expr, no_dedup) => {
        $crate::counted_ringbuf!(__RINGBUF, $t, $n, $init);
    };
    ($t:ident, $n:expr, $init:expr) => {
        $crate::counted_ringbuf!(__RINGBUF, $t, $n, $init);
    };
}

/// Inserts data into a named ringbuffer (which should have been declared with
/// the [`ringbuf!`] or [`counted_ringbuf!`] macro).
///
/// `ringbuf_entry!(NAME, expr)` will insert `expr` into the ringbuffer called
/// `NAME`.
///
/// If you declared your ringbuffer without a name, you can also use this
/// without a name, and it will default to `__RINGBUF`.
#[macro_export]
macro_rules! ringbuf_entry {
    ($buf:expr, $payload:expr) => {{
        // Evaluate both buf and payload, without letting them access each
        // other, by evaluating them in a tuple where each cannot
        // accidentally use the other's binding.
        let (p, buf) = ($payload, &$buf);
        // Invoke these functions using slightly weird syntax to avoid
        // accidentally calling a _different_ routine called record_entry.
        $crate::RecordEntry::record_entry(buf, line!() as u16, p);
    }};
    ($payload:expr) => {
        $crate::ringbuf_entry!(__RINGBUF, $payload);
    };
}

/// Inserts data into a ringbuffer at the root of this crate (which should have
/// been declared with the [`ringbuf!`] or [`counted_ringbuf!`] macro).
///
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! ringbuf_entry_root {
    ($payload:expr) => {
        $crate::ringbuf_entry!(crate::__RINGBUF, $payload);
    };
    ($buf:ident, $payload:expr) => {
        $crate::ringbuf_entry!(crate::$buf, $payload);
    };
}

///
/// The structure of a single [`Ringbuf`] entry, carrying a payload of arbitrary
/// type.  When a ring buffer entry is generated with an identical payload to
/// the most recent entry (in terms of both `line` and `payload`), `count` will
/// be incremented rather than generating a new entry.
///
#[derive(Debug, Copy, Clone)]
pub struct RingbufEntry<T: Copy, C> {
    pub line: u16,
    pub generation: u16,
    pub payload: T,
    pub count: C,
}

///
/// A ring buffer of parametrized type and size.  In practice, instantiating
/// this directly is strange -- see the [`ringbuf!`] macro.
///
#[derive(Debug)]
pub struct Ringbuf<T: Copy, C, const N: usize> {
    pub last: Option<usize>,
    pub buffer: [RingbufEntry<T, C>; N],
}

///
/// A ring buffer of parametrized type and size, plus counters tracking the
/// total number of times each entry variant has been recorded.
///
/// Event counts are incremented each time an entry is added to this ring
/// buffer. Counts are still tracked when the `disabled` feature is enabled. In
/// order to be counted, the entry type must implement the [`Count`] trait. See
/// [the crate-level documentation](crate#counted-ring-buffers) for more
/// details on recording entry counts.
///
/// In practice, instantiating this directly is strange -- see the
/// [`counted_ringbuf!`] macro.
///
#[cfg(feature = "counters")]
pub struct CountedRingbuf<T: Count + Copy, C, const N: usize> {
    /// A ring buffer of the `N` most recent entries recorded by this
    /// `CountedRingbuf`.
    #[cfg(not(feature = "disabled"))]
    pub ringbuf: StaticCell<Ringbuf<T, C, N>>,

    #[cfg(feature = "disabled")]
    pub _c: core::marker::PhantomData<fn(C)>,

    /// Counts of the total number of times each variant of `T` has been
    /// recorded, as defined by `T`'s [`Count`] impl.
    pub counters: T::Counters,
}

///
/// An abstraction over types in which ring buffer entries can be recorded.
///
/// This trait allows the [`ringbuf_entry!] and [`ringbuf_entry_root!`] macros
/// to record entries in both [`CountedRingbuf`]s and [`Ringbuf`]s without entry
/// total counters. It is implemented for the following types:
///
/// - [`CountedRingbuf`]`<T, N>`: used by ringbufs declared using the
///   [`counted_ringbuf!`] macro. This implementation increments the count of
///   the recorded entry variant, and (if the "disabled") feature flag is not
///   set, records the entry in the ringbuf.
/// - [`StaticCell`]`<`[`Ringbuf`]`<T, N>>`: used by ringbufs declared using the
///   [`ringbuf!`] macro, when the "disabled" feature flag is not enabled.
/// - `()`: used by ringbufs declared using the [`ringbuf!`] macro, when the
///   "disabled" feature flag is enabled. This implementation is a no-op.
///
/// It's typically unnecessary to implement this trait for other types, as its
/// only purpose is to allow the [`ringbuf_entry!`] and [`ringbuf_entry_root!`]
/// macros to dispatch based on which ringbuf type is being used.
pub trait RecordEntry<T: Copy> {
    /// Record a `T`-typed entry in this ringbuf. The `line` parameter should be
    /// the source code line on which the entry was recorded.
    ///
    /// This method is typically called by the [`ringbuf_entry!`] and
    /// [`ringbuf_entry_root!`] macros. While you could also call this method
    /// directly, [`ringbuf_entry!`] will capture the line number for you.
    fn record_entry(&self, line: u16, payload: T);
}

impl<T: Copy + PartialEq, const N: usize> RecordEntry<T>
    for StaticCell<Ringbuf<T, u16, { N }>>
{
    fn record_entry(&self, line: u16, payload: T) {
        // If the ringbuf is already borrowed, just do nothing, to avoid
        // panicking. This *shouldn't* ever happen, since we are
        // single-threaded, and the code for recording ringbuf entries won't
        // attempt to borrow the ringbuf twice...but, there's no nice way to
        // guarantee this.
        let Some(mut ring) = self.try_borrow_mut() else {
            return;
        };
        // If this is the first time this ringbuf has been poked, last will be
        // None. In this specific case we want to make sure we don't add to the
        // count of an existing entry, and also that we deposit the first entry
        // in slot 0. From a code generation perspective, the cheapest thing to
        // do is to treat None as an out-of-range value:
        let last = ring.last.unwrap_or(usize::MAX);

        // Check to see if we can reuse the most recent entry. This uses get_mut
        // both to avoid checking an entry on the first insertion (see above),
        // and also to handle the case where last is somehow corrupted to point
        // out-of-range. This avoids a bounds check panic. In the event that
        // last _is_ corrupted, the behavior below will just start us over at 0.
        if let Some(ent) = ring.buffer.get_mut(last) {
            if ent.line == line && ent.payload == payload {
                // Only reuse this entry if we don't overflow the
                // count.
                if let Some(new_count) = ent.count.checked_add(1) {
                    ent.count = new_count;
                    return;
                }
            }
        }

        ring.do_record(last, line, 1, payload);
    }
}

impl<T: Copy, const N: usize> RecordEntry<T>
    for StaticCell<Ringbuf<T, (), { N }>>
{
    fn record_entry(&self, line: u16, payload: T) {
        // If the ringbuf is already borrowed, just do nothing, to avoid
        // panicking. This *shouldn't* ever happen, since we are
        // single-threaded, and the code for recording ringbuf entries won't
        // attempt to borrow the ringbuf twice...but, there's no nice way to
        // guarantee this.
        let Some(mut ring) = self.try_borrow_mut() else {
            return;
        };
        // If this is the first time this ringbuf has been poked, last will be
        // None. In this specific case we want to make sure we don't add to the
        // count of an existing entry, and also that we deposit the first entry
        // in slot 0. From a code generation perspective, the cheapest thing to
        // do is to treat None as an out-of-range value:
        let last = ring.last.unwrap_or(usize::MAX);
        ring.do_record(last, line, (), payload);
    }
}

#[cfg(feature = "counters")]
impl<T, C, const N: usize> RecordEntry<T> for CountedRingbuf<T, C, { N }>
where
    T: Count + Copy,
    StaticCell<Ringbuf<T, C, N>>: RecordEntry<T>,
{
    fn record_entry(&self, _line: u16, payload: T) {
        payload.count(&self.counters);

        #[cfg(not(feature = "disabled"))]
        self.ringbuf.record_entry(_line, payload)
    }
}

impl<T> RecordEntry<T> for ()
where
    T: Copy + PartialEq,
{
    fn record_entry(&self, _: u16, _: T) {}
}

impl<T: Copy, C, const N: usize> Ringbuf<T, C, N> {
    fn do_record(&mut self, last: usize, line: u16, count: C, payload: T) {
        // Either we were unable to reuse the entry, or the last index was out
        // of range (perhaps because this is the first insertion). We're going
        // to advance last and wrap if required. This uses a wrapping_add
        // because if last is usize::MAX already, we want it to wrap to zero
        // regardless -- and this avoids a checked arithmetic panic on the +1.
        let ndx = {
            let last_plus_1 = last.wrapping_add(1);
            // You're probably wondering why this isn't a remainder operation.
            // This is because none of our target platforms currently have
            // hardware modulus, and many of them don't even have hardware
            // divide, making remainder quite expensive.
            if last_plus_1 >= self.buffer.len() {
                0
            } else {
                last_plus_1
            }
        };
        let ent = unsafe {
            // Safety: the code above guarantees that `ndx` is within the length
            // of the buffer --- we checked whether it's greater than or equal
            // to `self.buffer.len()` just a couple instructions ago. Thus,
            // unchecked indexing is fine here, and lets us avoid a panic.
            //
            // We could, alternatively, avoid the unsafe code by doing
            // `self.buffer.get_mut(ndx)` and then silently nop'ing if it
            // returns `None`...but it seems nicer to also elide the bounds
            // check, given that we *just* did one of our own!
            self.buffer.get_unchecked_mut(ndx)
        };
        *ent = RingbufEntry {
            line,
            payload,
            count,
            generation: ent.generation.wrapping_add(1),
        };

        self.last = Some(ndx);
    }
}
