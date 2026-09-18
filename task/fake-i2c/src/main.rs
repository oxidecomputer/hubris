// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A fake I2C driver, serving simulated devices.
//!
//! This task speaks the same protocol as the real I2C servers (see
//! `drv-i2c-api`): a `WriteRead` or `WriteReadBlock` operation carries the
//! device address and bus in its message and one or more write/read lease
//! pairs. Instead of a bus, requests are answered from a small table of
//! simulated devices, so that clients such as `thermal` and `sensor` can run
//! on the host, or anywhere without the hardware, with realistic traffic.
//!
//! Every write and read goes through the leases exactly as the real driver
//! does, which makes this a thorough exercise of the kernel's borrow
//! syscalls.
//!
//! The simulated bus currently holds one PCT2075/LM75 temperature sensor at
//! address 0x48, whose temperature drifts with kernel time so a control loop
//! has something to react to.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use drv_i2c_api::{Marshal, Op, ResponseCode};
use userlib::hl::{Borrow, Caller};
use userlib::{hl, sys_get_timer};

/// Upper bound on a single write or read, matching the real drivers.
const MAX_TRANSFER: usize = 255;

/// A simulated device: given the bytes written, produce the bytes read.
trait Device {
    /// Answers one write-then-read exchange. `read` is the number of bytes
    /// the client asked for (or the SMBus block maximum); returns the bytes
    /// to deliver, at most `read` of them.
    fn exchange(
        &mut self,
        written: &[u8],
        read: usize,
        block: bool,
        out: &mut [u8],
    ) -> Result<usize, ResponseCode>;
}

/// PCT2075 / LM75-family temperature sensor.
///
/// Register 0 holds the temperature as a 16-bit signed value in units of
/// 1/256 degrees, most significant byte first; the real part only uses the
/// top bits. The simulated temperature ramps between 30 and 45 degrees over
/// virtual time, one degree every two seconds.
struct Pct2075 {
    pointer: u8,
}

impl Device for Pct2075 {
    fn exchange(
        &mut self,
        written: &[u8],
        read: usize,
        block: bool,
        out: &mut [u8],
    ) -> Result<usize, ResponseCode> {
        if block {
            return Err(ResponseCode::OperationNotSupported);
        }
        if let Some(&reg) = written.first() {
            self.pointer = reg;
        }
        let bytes: [u8; 2] = match self.pointer {
            0 => {
                let degrees = 30 + (sys_get_timer().now / 2000) % 15;
                [degrees as u8, 0]
            }
            1 => [0, 0],      // configuration
            2 => [75 / 2, 0], // hysteresis
            3 => [80 / 2, 0], // overtemperature shutdown
            _ => return Err(ResponseCode::NoRegister),
        };
        let n = read.min(bytes.len());
        out[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }
}

/// The simulated bus, keyed by device address. Every controller, port and
/// mux segment sees the same devices.
fn device_at(address: u8) -> Option<&'static mut dyn Device> {
    static mut PCT2075: Pct2075 = Pct2075 { pointer: 0 };
    match address {
        // Safety: this task is single-threaded and this is the only place
        // that touches the static.
        0x48 => Some(unsafe { &mut *core::ptr::addr_of_mut!(PCT2075) }),
        _ => None,
    }
}

/// Performs the lease pairs of one request against a device, returning the
/// total number of bytes read.
fn serve(
    device: &mut dyn Device,
    caller: &Caller<usize>,
    lease_count: usize,
    op: Op,
) -> Result<usize, ResponseCode> {
    let mut total = 0;
    for i in (0..lease_count).step_by(2) {
        let wbuf: Borrow<'_> = caller.borrow(i);
        let winfo = wbuf.info().ok_or(ResponseCode::BadArg)?;
        if !winfo.attributes.contains(userlib::LeaseAttributes::READ) {
            return Err(ResponseCode::BadArg);
        }
        let rbuf = caller.borrow(i + 1);
        let rinfo = rbuf.info().ok_or(ResponseCode::BadArg)?;
        if winfo.len == 0 && rinfo.len == 0 {
            return Err(ResponseCode::BadArg);
        }
        if winfo.len > MAX_TRANSFER || rinfo.len > MAX_TRANSFER {
            return Err(ResponseCode::BadArg);
        }

        let mut written = [0u8; MAX_TRANSFER];
        wbuf.read_fully_at(0, &mut written[..winfo.len])
            .ok_or(ResponseCode::BadArg)?;

        let block = op == Op::WriteReadBlock && i == lease_count - 2;
        let mut out = [0u8; MAX_TRANSFER];
        let n = device.exchange(
            &written[..winfo.len],
            rinfo.len,
            block,
            &mut out[..rinfo.len],
        )?;
        rbuf.write_fully_at(0, &out[..n])
            .ok_or(ResponseCode::BadArg)?;
        total += n;
    }
    Ok(total)
}

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    let mut buffer = [0u8; 4];
    loop {
        hl::recv_without_notification(&mut buffer, |op, msg| match op {
            Op::WriteRead | Op::WriteReadBlock => {
                let lease_count = msg.lease_count();
                let (payload, caller) = msg
                    .fixed::<[u8; 4], usize>()
                    .ok_or(ResponseCode::BadArg)?;
                if lease_count < 2 || !lease_count.is_multiple_of(2) {
                    return Err(ResponseCode::IllegalLeaseCount);
                }
                let (address, _controller, _port, _segment) =
                    Marshal::unmarshal(payload)?;
                let device =
                    device_at(address).ok_or(ResponseCode::NoDevice)?;
                let total = serve(device, &caller, lease_count, op)?;
                caller.reply(total);
                Ok(())
            }
            #[allow(unreachable_patterns)]
            _ => Err(ResponseCode::OperationNotSupported),
        });
    }
}
