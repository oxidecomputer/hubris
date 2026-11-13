// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Supervisor for host test images.
//! robo:
//! robo: Unlike jefe, it never restarts a faulted task: any fault fails the
//! robo: run. The orchestrator ends a passing run, and restarts tasks on
//! robo: purpose, through [`host_test_api::supervisor`].

use host_test_api::control::{CONTROL_OP, Control, handle_control};
use host_test_api::supervisor::Request;
use userlib::{
    PANIC_MESSAGE_MAX_LEN, ReplyFaultReason, TaskId, hl, kipc, sys_get_timer,
    sys_recv_open, sys_reply_fault,
};

struct Supervisor;

impl Control for Supervisor {
    type Request = Request;
    type Response = ();

    fn control(&mut self, req: Request) {
        match req {
            Request::Pass => {
                eprintln!("host-test: PASS at t={}", sys_get_timer().now);
                kipc::system_restart()
            }
            Request::RestartTask { index } => {
                kipc::reinit_task(usize::from(index), true)
            }
        }
    }
}

/// robo: Prints every faulted task and halts, which fails the run.
fn fail_on_faults() -> ! {
    let mut next = 0;
    while let Some(i) = kipc::find_faulted_task(next) {
        let i = i.get();
        eprintln!(
            "host-test: FAIL at t={}: task {i} faulted: {:?}",
            sys_get_timer().now,
            kipc::read_task_status(i),
        );
        let mut buf = [0u8; PANIC_MESSAGE_MAX_LEN];
        if let Ok(msg) = kipc::read_panic_message(i, &mut buf) {
            let msg: String = msg.map(|c| c.valid()).collect();
            eprintln!("host-test: task {i} panicked: {msg}");
        }
        next = i + 1;
    }
    hl::halt()
}

fn main() -> ! {
    let mut buffer = [0u8; 256];
    loop {
        let rm = sys_recv_open(&mut buffer, notifications::FAULT_MASK);
        if rm.sender == TaskId::KERNEL {
            fail_on_faults();
        }
        let Some(msg) = buffer.get(..rm.message_len) else {
            sys_reply_fault(rm.sender, ReplyFaultReason::BadMessageSize);
            continue;
        };
        if rm.operation == u32::from(CONTROL_OP) {
            handle_control(&mut Supervisor, msg, &rm);
        } else {
            sys_reply_fault(rm.sender, ReplyFaultReason::UndefinedOperation);
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
