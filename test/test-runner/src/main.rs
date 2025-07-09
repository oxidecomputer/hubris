// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Generic test suite runner task.
//!
//! # Architecture
//!
//! This task is intended to play the "supervisor" role in test images. It
//! receives notification of test status from another task which runs the
//! actual tests. The actual triggering of the tests comes from another
//! entity (currently hiffy)
//!
//! This task should be index 0, while the testsuite should be index 1.
//!
//!```text
//!Test Suite              Test Requester              Test supervisor
//!                         (currently hiffy)                  +
//!                               +                            |
//!                               |                            |
//!                               |                            |
//!         Run test N            |                            |
//!     <-------------------------++                           |
//!     |                          |                 +-------> |
//!     |                          |                 +         |
//!     |     Ok will do           |   Waits for notifications |
//!     +------------------------->+   and faults etc.         |
//!     |                          |                           |
//!     |                          |                           |
//!     |  <--+  Does test things  |                           |
//!     |                          |       Done yet?           |
//!     |                          |                           |
//!     |                          +-------------------------->+
//!     |                          |                           |
//!     |                          |                           |
//!     |                          |                           |
//!     |       All done!          |                           |
//!     +----------------------+   |       +--------------------+
//!                            |   |       |                   |
//!                           ++-----------+                   |
//!                                |                           |
//!                                |                           |
//!                                |                           |
//!                                |    Here is the status    ++
//!                                ^---------------------------+
//!```

#![no_std]
#![no_main]
#![forbid(clippy::wildcard_imports)]

use ringbuf::{ringbuf, ringbuf_entry};
use test_api::{RunnerOp, TestResult};
use userlib::{hl, kipc, TaskId, TaskState};

/// We are sensitive to all notifications, to catch unexpected ones in test.
const ALL_NOTIFICATIONS: u32 = !0;

/// This runner is written such that the task under test must be task index 1.
/// (And the runner must be zero.)
const TEST_TASK: usize = 1;

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Notification,
    TestComplete(TaskId),
    TestResult(TaskId),
    SoftIrq(TaskId, u32),
    AutoRestart(bool),
    RestartingTask(usize),
    None,
}

ringbuf!(Trace, 64, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    struct MonitorState {
        received_notes: u32,
        test_status: Option<bool>,
        auto_restart: bool,
    }

    let mut state = MonitorState {
        received_notes: 0,
        test_status: None,
        auto_restart: false,
    };

    // N.B. that this must be at least four bytes to recv a u32 notification
    // mask in the `SoftIrq` IPC op.
    let mut buf = [0u8; 4];
    loop {
        hl::recv(
            &mut buf,
            ALL_NOTIFICATIONS,
            &mut state,
            |state, bits| {
                ringbuf_entry!(Trace::Notification);

                // Record all received notification bits.
                state.received_notes |= bits;

                if bits & 1 != 0 {
                    // Uh-oh, somebody faulted.
                    if find_and_report_fault() {
                        // It was the test.
                        state.test_status = Some(false);
                    }
                    if state.auto_restart {
                        restart_faulted_tasks();
                    }
                }
            },
            |state, op: RunnerOp, msg| -> Result<(), u32> {
                match op {
                    RunnerOp::ReadAndClearNotes => {
                        let (_, caller) = msg.fixed::<(), u32>().ok_or(2u32)?;
                        caller.reply(state.received_notes);
                        state.received_notes = 0;
                    }
                    RunnerOp::SoftIrq => {
                        // The test is asking us to trigger an IRQ.
                        let (&mask, caller) =
                            msg.fixed::<u32, ()>().ok_or(2u32)?;
                        ringbuf_entry!(Trace::SoftIrq(caller.task_id(), mask));
                        kipc::software_irq(caller.task_id().index(), mask);
                        caller.reply(())
                    }
                    RunnerOp::AutoRestart => {
                        let (&v, caller) =
                            msg.fixed::<u32, ()>().ok_or(0u32)?;
                        let auto_restart = v != 0;
                        ringbuf_entry!(Trace::AutoRestart(auto_restart));
                        state.auto_restart = auto_restart;
                        caller.reply(())
                    }
                    RunnerOp::TestComplete => {
                        let (_, caller) = msg.fixed::<(), ()>().ok_or(2u32)?;
                        ringbuf_entry!(Trace::TestComplete(caller.task_id()));
                        caller.reply(());
                        state.test_status = Some(true);
                    }
                    RunnerOp::TestResult => {
                        let (_, caller) = msg.fixed::<(), u32>().ok_or(2u32)?;
                        ringbuf_entry!(Trace::TestResult(caller.task_id()));
                        match state.test_status {
                            Some(r) => {
                                if r {
                                    caller.reply(TestResult::Success as u32);
                                } else {
                                    caller.reply(TestResult::Failure as u32);
                                }
                                state.test_status = None;
                            }
                            None => caller.reply(TestResult::NotDone as u32),
                        }
                    }
                }
                Ok(())
            },
        );
    }
}

/// Scans the kernel's task table looking for a task that has fallen over.
/// Prints any that are found.
///
/// If the testsuite is found to have fallen over, this function returns true.
/// The test suite is _not_ restarted to give a chance to collect task state
fn find_and_report_fault() -> bool {
    let mut tester_faulted = false;
    for i in 0..hubris_num_tasks::NUM_TASKS {
        let s = kipc::read_task_status(i);
        if let TaskState::Faulted { .. } = s {
            if i == TEST_TASK {
                tester_faulted = true;
            }
        }
    }
    tester_faulted
}

/// Restart faulted tasks (other than the test suite task)
fn restart_faulted_tasks() {
    for i in 0..hubris_num_tasks::NUM_TASKS {
        let s = kipc::read_task_status(i);
        if let TaskState::Faulted { .. } = s {
            if i != TEST_TASK {
                ringbuf_entry!(Trace::RestartingTask(i));
                kipc::reinit_task(i, true);
            }
        }
    }
}
