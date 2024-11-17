// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An I2C device emulator for Sidecar Mainboard, intended to convince the
//! sequencer task it is running on a Sidecar Mainboard rather than say a
//! Gimletlet.

#![no_std]
#![no_main]

use drv_i2c_api::{I2cDevice, ResponseCode};
use ringbuf::*;
use userlib::*;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    Addr(u8),
}
ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let mut buffer = [0; 4];

    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::WriteRead | Op::WriteReadBlock => {
                let (payload, caller) = msg
                    .fixed_with_leases::<[u8; 4], usize>(2)
                    .ok_or(ResponseCode::BadArg)?;

                let (addr, _, _, _) = Marshal::unmarshal(payload)?;

                if let Some(_) = ReservedAddress::from_u8(addr) {
                    return Err(ResponseCode::ReservedAddress);
                }

                ringbuf_entry!(Trace::Addr(addr));

                let wbuf = caller.borrow(0);
                let winfo = wbuf.info().ok_or(ResponseCode::BadArg)?;

                if !winfo.attributes.contains(LeaseAttributes::READ) {
                    return Err(ResponseCode::BadArg);
                }

                let rbuf = caller.borrow(1);
                let rinfo = rbuf.info().ok_or(ResponseCode::BadArg)?;

                if winfo.len == 0 && rinfo.len == 0 {
                    // We must have either a write OR a read -- while perhaps
                    // valid to support both being zero as a way of testing an
                    // address for a NACK, it's not a mode that we (currently)
                    // support.
                    return Err(ResponseCode::BadArg);
                }

                if winfo.len > 255 || rinfo.len > 255 {
                    // For now, we don't support writing or reading more than
                    // 255 bytes.
                    return Err(ResponseCode::BadArg);
                }

                // Send an empty reply back to the caller.
                caller.reply(0);
                Ok(())
            }
        });
    }
}
