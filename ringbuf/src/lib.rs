//! Ring buffer for debugging Hubris tasks and drivers
//!
//! This contains an implementation for a static, global ring buffer designed
//! to be used to instrument arbitrary contexts.  While there is nothing to
//! prevent these ring buffers from being left in production code, the design
//! center is primarily around debugging in development: the ring buffers
//! themselves can be processed either with Humility (which has built-in
//! support via the `humility ringbuf` command) or via GDB.
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
//! ADDR        NDX LINE  GEN    COUNT PAYLOAD
//! 0x200082f8   47   91   13        1 (22.0625, Some(TempMSB))
//! 0x20008308   48   91   13       19 (22, Some(TempMSB))
//! 0x20008318   49   91   13        1 (22.0625, Some(TempMSB))
//! 0x20008328   50   91   13        1 (22, Some(TempMSB))
//! 0x20008338   51   91   13        3 (22.0625, Some(TempMSB))
//! 0x20008348   52   91   13       12 (22, Some(TempMSB))
//! 0x20008358   53   91   13        2 (22.0625, Some(TempMSB))
//! 0x20008368   54   91   13        2 (22, Some(TempMSB))
//! ...
//! ```
//!
//! If for any reason a raw view is needed, one can also use `humility readvar`
//! and specify the corresponding `RINGBUF` variable.
//!
//! ## Inspecting a ring buffer via GDB
//!
//! Assuming symbols are loaded, one can use GDB's `print` command,
//! specifying the task that contains the ring buffer and the `RINGBUF`
//! variable, e.g. to inspect a ring buffer in the `task_adt7420` task:
//!
//! ```console
//! (gdb) set print pretty on
//! (gdb) print task_adt7420::RINGBUF
//!
//! $3 = ringbuf::Ringbuf<(f32, core::option::Option<task_adt7420::Register>)> {
//!  last: core::option::Option<usize>::Some(21),
//!  buffer: [
//!    ringbuf::RingbufEntry<(f32, core::option::Option<task_adt7420::Register>)> {
//!      line: 91,
//!      generation: 15,
//!      count: 1,
//!      payload: (
//!        22.0625,
//!        core::option::Option<task_adt7420::Register>::Some(task_adt7420::Register::TempMSB)
//!      )
//!    },...
//! ```

#![no_std]

///
/// The structure of a single [`Ringbuf`] entry, carrying a payload of
/// arbitrary type.  When a ring buffer entry is generated with an identical
/// payload to the most recent entry (in terms of both `line` and `payload`),
/// `count` will be incremented rather than generating a new entry.
///
#[derive(Debug, Copy, Clone)]
pub struct RingbufEntry<T: Copy + PartialEq> {
    pub line: u16,
    pub generation: u16,
    pub count: u32,
    pub payload: T,
}

///
/// A ring buffer of parametrized type and size.  This should be instantiated
/// with the [`ringbuf!`] macro.
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
                    ent.count += 1;
                    return;
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
        ent.generation += 1;

        self.last = Some(ndx);
    }
}

///
/// Defines a static ring buffer with a payload type of `$ptype` and `$size`
/// entries.  Because the ring buffer is static, `$pinit` must be provided to
/// statically initialize the payloads of the ring buffer.  An entry is recorded
/// in the ring buffer with a call to [`ringbuf_entry!`].
///
#[macro_export]
macro_rules! ringbuf {
    ($ptype:ty, $size:tt, $pinit:tt) => {
        #[no_mangle]
        static mut RINGBUF: Ringbuf<$ptype, $size> = Ringbuf::<$ptype, $size> {
            last: None,
            buffer: [RingbufEntry {
                line: 0,
                generation: 0,
                count: 0,
                payload: $pinit,
            }; $size],
        };
    };
}

///
/// Adds an entry to a ring buffer that has been declared with [`ringbuf!`].
/// The line number of the call will be recorded, along with the payload.  If
/// the ring buffer is full, the oldest entry in the ring buffer will be
/// overwritten.  If the line number and the payload both match the most
/// recent entry in the ring buffer, no new entry will be added, and the count
/// of the last entry will be incremented.
///
#[macro_export]
macro_rules! ringbuf_entry {
    ($payload:expr) => {
        let ringbuf = unsafe { &mut RINGBUF };
        ringbuf.entry(line!() as u16, $payload);
    };
}
