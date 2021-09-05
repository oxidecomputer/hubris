//! Ring buffer for debugging Hubris tasks and drivers
//!
//! This contains an implementation for a static, global ring buffer designed
//! to be used to instrument arbitrary contexts.  While there is nothing to
//! prevent these ring buffers from being left in production code, the design
//! center is primarily around debugging in development: the ring buffers
//! themselves can be processed either with Humility (which has built-in
//! support via the `humility ringbuf` command) or via GDB.
//!
//! ## Constraints
//!
//! There are several important constraints for a ring buffer:
//!
//! 1. Only one ring buffer is permitted per file
//! 2. The type in the ring buffer must implement both `Copy` and `PartialEq`
//! 3. The generated code relies on `min_const_generics`
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

#![feature(proc_macro_span)]

extern crate proc_macro;
use proc_macro::{Span, TokenStream};
use quote::format_ident;
use quote::quote;
use syn::parse::{Parse, ParseStream, Result};
use syn::parse_macro_input;
use syn::{Error, Expr, LitInt, Token, Type};

struct RingbufParams {
    ptype: Type,
    size: LitInt,
    pinit: Expr,
}

impl Parse for RingbufParams {
    fn parse(input: ParseStream) -> Result<Self> {
        let ptype: Type = input.parse()?;
        input.parse::<Token![,]>()?;
        let size: LitInt = input.parse()?;

        if size.base10_parse::<u32>()? == 0 {
            Err(Error::new(size.span(), "ring buffer size cannot be 0"))
        } else {
            input.parse::<Token![,]>()?;
            let pinit: Expr = input.parse()?;

            Ok(RingbufParams { ptype, size, pinit })
        }
    }
}

///
/// Defines a static ring buffer with a payload type of `ptype` and `size`
/// entries.  Because the ring buffer is static, `pinit` must be provided to
/// statically initialize the payloads of the ring buffer.  An entry is recorded
/// in the ring buffer with a call to [`ringbuf_entry!`].
///
#[proc_macro]
pub fn ringbuf(input: TokenStream) -> TokenStream {
    let params = parse_macro_input!(input as RingbufParams);

    //
    // If the ring buffer is disabled, we want to merely emit an implementation
    // of ringbuf_entry! that does nothing.
    //
    if cfg!(feature = "disabled") {
        let ringbuf = quote! {
            macro_rules! ringbuf_entry {
                ($payload:expr) => {}
            }
        };

        return ringbuf.into();
    }

    let ptype = params.ptype;
    let size = params.size;
    let pinit = params.pinit;

    //
    // A little sleazy: if the file is main.rs or lib.rs, we'll use the directory
    // name of the crate to form our ringbuf identifier -- otherwise we'll use
    // the stem of our filename.
    //
    let path = Span::call_site().source_file().path();
    let file = path.file_name().unwrap().to_string_lossy();

    let prefix = if file == "main.rs" || file == "lib.rs" {
        let parent = path.parent().unwrap();
        let grandparent = parent.parent().unwrap();
        grandparent.file_name().unwrap()
    } else {
        path.file_stem().unwrap()
    };

    let upper = prefix.to_string_lossy().to_ascii_uppercase();
    let name = format_ident!("{}_RINGBUF", str::replace(&upper, "-", "_"));

    let ringbuf = quote! {
        ///
        /// The structure of a single [`Ringbuf`] entry, carrying a payload of
        /// arbitrary type.  When a ring buffer entry is generated with an
        /// identical payload to the most recent entry (in terms of both
        /// `line` and `payload`), `count` will be incremented rather than
        /// generating a new entry.
        ///
        #[derive(Debug, Copy, Clone)]
        struct RingbufEntry<T: Copy + PartialEq> {
            line: u16,
            generation: u16,
            count: u32,
            payload: T,
        }

        ///
        /// A ring buffer of parametrized type and size.  This should be
        /// instantiated with the [`ringbuf!`] macro.
        ///
        #[derive(Debug)]
        struct Ringbuf<T: Copy + PartialEq, const N: usize> {
            last: Option<usize>,
            buffer: [RingbufEntry<T>; N],
        }

        impl<T: Copy + PartialEq, const N: usize> Ringbuf<T, { N }> {
            fn entry(&mut self, line: u16, payload: T) {
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

        #[no_mangle]
        static mut #name: Ringbuf<#ptype, #size> = Ringbuf::<#ptype, #size> {
            last: None,
            buffer: [RingbufEntry {
                line: 0,
                generation: 0,
                count: 0,
                payload: #pinit,
            }; #size],
        };

        // See rustdoc for this, below
        macro_rules! ringbuf_entry {
            ($payload:expr) => {
                let ringbuf = unsafe { &mut #name };
                ringbuf.entry(line!() as u16, $payload);
            };
        }
    };

    ringbuf.into()
}

///
/// Adds an entry to a ring buffer that has been declared with [`ringbuf!`].
/// The line number of the call will be recorded, along with the payload.  If
/// the ring buffer is full, the oldest entry in the ring buffer will be
/// overwritten.  If the line number and the payload both match the most
/// recent entry in the ring buffer, no new entry will be added, and the count
/// of the last entry will be incremented.
///
#[cfg(doc)]
#[proc_macro]
pub fn ringbuf_entry(input: TokenStream) -> TokenStream {
    // In order to generate a doc comment for the code that we generate (that
    // is, for [`ringbuf_entry!`]), we have this bogus definition of our macro
    // that just passes its input in the `#[cfg(doc)]` case; we are using it
    // only to generate the documentation above.
    input.into()
}
