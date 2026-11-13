// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Fake I2C server for host tests.
//! robo:
//! robo: Speaks the `drv-i2c-api` `WriteRead`/`WriteReadBlock` protocol.
//! robo: Devices are register files attached by the orchestrator (see
//! robo: [`host_test_api::i2c`]); anything else NACKs. Every transfer is
//! robo: logged.

use std::collections::BTreeMap;

use drv_i2c_api::{Op, ResponseCode};
use host_test_api::control::{CONTROL_OP, Control, handle_control};
use host_test_api::i2c::{Addr, Event, Request, Response};
use userlib::{
    FromPrimitive, LeaseAttributes, RecvMessage, ReplyFaultReason,
    sys_borrow_info, sys_borrow_read, sys_borrow_write, sys_get_timer,
    sys_recv_open, sys_reply, sys_reply_fault,
};

/// robo: Most events returned per control reply.
const EVENTS_PER_REPLY: usize = 256;

/// robo: Read length the real server rejects above, for either lease.
const MAX_XFER: usize = 255;

#[derive(Default)]
struct Device {
    regs: BTreeMap<u8, Vec<u8>>,
}

#[derive(Default)]
struct ServerImpl {
    devices: BTreeMap<Addr, Device>,
    events: Vec<Event>,
}

impl ServerImpl {
    /// robo: One write/read pair. Returns the bytes read.
    fn transfer(
        &mut self,
        addr: &Addr,
        write: &[u8],
        read_len: usize,
        block: bool,
    ) -> Result<Vec<u8>, ResponseCode> {
        let dev = self.devices.get_mut(addr).ok_or(ResponseCode::NoDevice)?;
        let Some((&cmd, data)) = write.split_first() else {
            // robo: A read with no register select; nothing uses this yet.
            return Err(ResponseCode::NoRegister);
        };

        if read_len == 0 {
            // robo: A write. `[cmd]` alone is a PMBus send-byte (e.g.
            // robo: CLEAR_FAULTS) and only gets logged.
            if !data.is_empty() {
                dev.regs.insert(cmd, data.to_vec());
            }
            return Ok(Vec::new());
        }

        let reg = dev.regs.get(&cmd).ok_or(ResponseCode::NoRegister)?;
        let mut out = reg.clone();
        if block {
            // robo: The block length byte isn't passed back to the caller;
            // robo: anything that doesn't fit is dropped.
            out.truncate(read_len);
        } else {
            out.resize(read_len, 0xff);
        }
        Ok(out)
    }

    fn handle_write_read(&mut self, rm: &RecvMessage, op: Op, msg: &[u8]) {
        let Ok(addr) = <Addr>::try_from(msg) else {
            sys_reply(rm.sender, ResponseCode::BadArg as u32, &[]);
            return;
        };
        let leases = rm.lease_count;
        if leases < 2 || !leases.is_multiple_of(2) {
            sys_reply(rm.sender, ResponseCode::IllegalLeaseCount as u32, &[]);
            return;
        }

        let mut total = 0usize;
        for i in (0..leases).step_by(2) {
            let (Some(winfo), Some(rinfo)) = (
                sys_borrow_info(rm.sender, i),
                sys_borrow_info(rm.sender, i + 1),
            ) else {
                sys_reply(rm.sender, ResponseCode::BadArg as u32, &[]);
                return;
            };
            if !winfo.attributes.contains(LeaseAttributes::READ)
                || (winfo.len == 0 && rinfo.len == 0)
                || winfo.len > MAX_XFER
                || rinfo.len > MAX_XFER
            {
                sys_reply(rm.sender, ResponseCode::BadArg as u32, &[]);
                return;
            }

            let mut write = vec![0u8; winfo.len];
            let (rc, _) = sys_borrow_read(rm.sender, i, 0, &mut write);
            if rc != 0 {
                return;
            }

            let block = op == Op::WriteReadBlock && i == leases - 2;
            let result = self.transfer(&addr, &write, rinfo.len, block);
            self.events.push(Event {
                time: sys_get_timer().now,
                task: rm.sender.index() as u16,
                addr,
                write: write.clone(),
                read_len: rinfo.len,
                ok: result.is_ok(),
            });
            match result {
                Ok(data) => {
                    if !data.is_empty() {
                        let (rc, _) =
                            sys_borrow_write(rm.sender, i + 1, 0, &data);
                        if rc != 0 {
                            return;
                        }
                    }
                    total += data.len();
                }
                Err(code) => {
                    sys_reply(rm.sender, code as u32, &[]);
                    return;
                }
            }
        }
        sys_reply(rm.sender, 0, &total.to_ne_bytes());
    }
}

impl Control for ServerImpl {
    type Request = Request;
    type Response = Response;

    fn control(&mut self, req: Request) -> Response {
        match req {
            Request::Attach { addr, regs } => {
                self.devices.insert(
                    addr,
                    Device {
                        regs: regs.into_iter().collect(),
                    },
                );
                Response::Done
            }
            Request::Detach { addr } => {
                self.devices.remove(&addr);
                Response::Done
            }
            Request::SetRegister {
                addr,
                reg: (cmd, bytes),
            } => {
                if let Some(dev) = self.devices.get_mut(&addr) {
                    dev.regs.insert(cmd, bytes);
                }
                Response::Done
            }
            Request::Events { from } => Response::Events(
                self.events
                    .iter()
                    .skip(from)
                    .take(EVENTS_PER_REPLY)
                    .cloned()
                    .collect(),
            ),
        }
    }
}

fn main() -> ! {
    let mut server = ServerImpl::default();
    let mut buffer = [0u8; 1024];
    loop {
        let rm = sys_recv_open(&mut buffer, 0);
        let Some(msg) = buffer.get(..rm.message_len) else {
            sys_reply_fault(rm.sender, ReplyFaultReason::BadMessageSize);
            continue;
        };
        if rm.operation == u32::from(CONTROL_OP) {
            handle_control(&mut server, msg, &rm);
            continue;
        }
        match Op::from_u32(rm.operation) {
            Some(op @ (Op::WriteRead | Op::WriteReadBlock)) => {
                server.handle_write_read(&rm, op, msg);
            }
            _ => {
                sys_reply_fault(rm.sender, ReplyFaultReason::UndefinedOperation)
            }
        }
    }
}
