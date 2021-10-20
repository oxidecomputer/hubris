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
//! must implement both `Copy` and `PartialEq`.
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

/// Re-export the bits we use from `userlib` so that code generated by the
/// macros is guaranteed to be able to find them.
pub use userlib::util::StaticCell;

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
        #[used]
        static $name: $crate::StaticCell<$crate::Ringbuf<$t, $n>> =
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
    ($t:ty, $n:expr, $init:expr) => {
        $crate::ringbuf!(__RINGBUF, $t, $n, $init);
    };
}

#[cfg(feature = "disabled")]
#[macro_export]
macro_rules! ringbuf {
    ($name:ident, $t:ty, $n:expr, $init:expr) => {};
    ($t:ty, $n:expr, $init:expr) => {};
}

/// Inserts data into a named ringbuffer (which should have been declared with
/// the `ringbuf!` macro).
///
/// `ringbuf_entry!(NAME, expr)` will insert `expr` into the ringbuffer called
/// `NAME`.
///
/// If you declared your ringbuffer without a name, you can also use this
/// without a name, and it will default to `__RINGBUF`.
#[cfg(not(feature = "disabled"))]
#[macro_export]
macro_rules! ringbuf_entry {
    ($buf:expr, $payload:expr) => {{
        // Evaluate both buf and payload, without letting them access each
        // other, by evaluating them in a tuple where each cannot
        // accidentally use the other's binding.
        let (p, buf) = ($payload, &$buf);
        // Invoke these functions using slightly weird syntax to avoid
        // accidentally calling a _different_ routine called borrow_mut or
        // entry.
        $crate::Ringbuf::entry(
            &mut *$crate::StaticCell::borrow_mut(buf),
            line!() as u16,
            p,
        );
    }};
    ($payload:expr) => {
        $crate::ringbuf_entry!(__RINGBUF, $payload);
    };
}

#[cfg(feature = "disabled")]
#[macro_export]
macro_rules! ringbuf_entry {
    ($buf:expr, $payload:expr) => {};
    ($payload:expr) => {};
}

///
/// The structure of a single [`Ringbuf`] entry, carrying a payload of arbitrary
/// type.  When a ring buffer entry is generated with an identical payload to
/// the most recent entry (in terms of both `line` and `payload`), `count` will
/// be incremented rather than generating a new entry.
///
#[derive(Debug, Copy, Clone)]
pub struct RingbufEntry<T: Copy + PartialEq> {
    pub line: u16,
    pub generation: u16,
    pub count: u32,
    pub payload: T,
}

///
/// A ring buffer of parametrized type and size.  In practice, instantiating
/// this directly is strange -- see the [`ringbuf!`] macro.
///
#[derive(Debug)]
pub struct Ringbuf<T: Copy + PartialEq, const N: usize> {
    pub last: Option<usize>,
    pub buffer: [RingbufEntry<T>; N],
}

impl<T: Copy + PartialEq, const N: usize> Ringbuf<T, { N }> {
    pub fn entry(&mut self, line: u16, payload: T) {
        let ndx = match self.last {
            None => 0,
            Some(last) => {
                let ent = &mut self.buffer[last];

                if ent.line == line && ent.payload == payload {
                    // Only reuse this entry if we don't overflow the
                    // count.
                    if let Some(new_count) = ent.count.checked_add(1) {
                        ent.count = new_count;
                        return;
                    }
                }

                if last + 1 >= self.buffer.len() {
                    0
                } else {
                    last + 1
                }
            }
        };

        let ent = &mut self.buffer[ndx];
        ent.line = line;
        ent.payload = payload;
        ent.count = 1;
        ent.generation = ent.generation.wrapping_add(1);

        self.last = Some(ndx);
    }
}
