// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: The control operation, and a server loop that serves it next to an
//! robo: idol interface.
//! robo:
//! robo: Control messages use one operation number, [`CONTROL_OP`], chosen to
//! robo: be far above any idol operation number. The request and reply are
//! robo: `postcard`-encoded.

use core::marker::PhantomData;

use idol_runtime::{RequestError, Server, ServerOp};
use serde::{Serialize, de::DeserializeOwned};
use userlib::{
    RecvMessage, ReplyFaultReason, TaskId, sys_recv_open, sys_reply,
    sys_reply_fault, sys_send,
};

/// robo: Operation number used for control messages.
pub const CONTROL_OP: u16 = 0xC0DE;

/// robo: Largest control reply, in bytes.
pub const MAX_REPLY: usize = 64 * 1024;

/// robo: A fake's handler for control messages.
pub trait Control {
    type Request: DeserializeOwned;
    type Response: Serialize;

    fn control(&mut self, req: Self::Request) -> Self::Response;
}

/// robo: Sends a control request to `task` and returns its reply.
/// robo:
/// robo: Panics if the fake rejects the message; that is a fixture bug.
pub fn call<Req: Serialize, Resp: DeserializeOwned>(
    task: TaskId,
    req: &Req,
) -> Resp {
    let out = postcard::to_stdvec(req).expect("encoding control request");
    let mut reply = vec![0u8; MAX_REPLY];
    let task = userlib::sys_refresh_task_id(task);
    let (code, len) = sys_send(task, CONTROL_OP, &out, &mut reply, &[]);
    if code != 0 {
        panic!("control call to task {} failed: {code:#x}", task.index());
    }
    postcard::from_bytes(&reply[..len]).expect("decoding control reply")
}

/// robo: Receives and handles one message: a control message, or an
/// robo: operation of the idol interface `Op`.
/// robo:
/// robo: The idol path repeats the checks in `idol_runtime::dispatch`, which
/// robo: can't be used here because it does its own receive.
pub fn serve_one<S, Op>(buffer: &mut [u8], server: &mut S)
where
    S: Control,
    Op: ServerOp,
    for<'a> (PhantomData<Op>, &'a mut S): Server<Op>,
{
    let rm = sys_recv_open(buffer, 0);
    let Some(incoming) = buffer.get(..rm.message_len) else {
        sys_reply_fault(rm.sender, ReplyFaultReason::BadMessageSize);
        return;
    };

    if rm.operation == u32::from(CONTROL_OP) {
        handle_control(server, incoming, &rm);
        return;
    }

    let Some(op) = Op::from_u32(rm.operation) else {
        sys_reply_fault(rm.sender, ReplyFaultReason::UndefinedOperation);
        return;
    };
    if rm.response_capacity < op.max_reply_size() {
        sys_reply_fault(rm.sender, ReplyFaultReason::ReplyBufferTooSmall);
        return;
    }
    if rm.lease_count != op.required_lease_count() {
        sys_reply_fault(rm.sender, ReplyFaultReason::BadLeases);
        return;
    }

    let mut pair = (PhantomData::<Op>, server);
    match pair.handle(op, incoming, &rm) {
        Ok(()) => {}
        Err(RequestError::Runtime(code)) => {
            sys_reply(rm.sender, u32::from(code), &[]);
        }
        Err(RequestError::Fail(e)) => {
            if let Some(reason) = e.into_fault() {
                sys_reply_fault(rm.sender, reason);
            }
        }
    }
}

/// robo: Decodes, handles, and replies to one control message.
pub fn handle_control<S: Control>(
    server: &mut S,
    incoming: &[u8],
    rm: &RecvMessage,
) {
    let Ok(req) = postcard::from_bytes(incoming) else {
        sys_reply_fault(rm.sender, ReplyFaultReason::BadMessageContents);
        return;
    };
    let resp = server.control(req);
    let out = postcard::to_stdvec(&resp).expect("encoding control reply");
    if out.len() > rm.response_capacity {
        sys_reply_fault(rm.sender, ReplyFaultReason::ReplyBufferTooSmall);
        return;
    }
    sys_reply(rm.sender, 0, &out);
}
