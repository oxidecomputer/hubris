// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Generic test suite runner task.
//!
//! # Architecture
//!
//! This task is intended to play the "supervisor" role in test images. It
//! controls execution of another task, the "testsuite," which contains the
//! actual tests. (A test image can also contain other tasks as needed.)
//!
//! This task should be index 0, while the testsuite should be index 1.
//!
//! A test _suite_ consists of one or more test _cases_, which are run
//! individually and can fail separately.
//!
//! The test protocol assumes that the testsuite is message-driven. The
//! interaction between the two tasks is as follows:
//!
//! ```text
//! runner                      testsuite
//!   |                             *  <-- blocks in RECV
//!   |                             :
//!   | test metadata request       :  \
//!   +---------------------------->+  | repeats for each
//!   :      test metadata response |  | test case in suite
//!   +<----------------------------+  /
//!   |
//!   | run test case N
//!   +---------------------------->+
//!   :                  ok will do |
//!   +<----------------------------+
//!   |                             | running
//!   * <-- blocks in RECV          | test
//!   :             service request | code...
//!   +<----------------------------+ \
//!   | service response            : | test can make 0 or more service calls
//!   +---------------------------->+ /
//!   |                             |
//!   * <-- blocks in RECV          | more test code...
//!   :          test case complete |
//!   +<----------------------------+ \
//!   | acknowledge                 : | until it reports the test is done.
//!   +---------------------------->+ /
//!   |                             |
//!   |                             * <-- blocks in RECV
//!   |
//!   and so on
//! ```
//!
//! The key detail in the diagram above: the runner and the testsuite *switch
//! roles* in terms of who calls who.
//!
//! - Between tests, the runner does the calling, to get metadata and eventually
//!   ask for a test to start.
//! - While the test is running, the runner listens for messages. The testsuite
//!   may call the runner at this point to request services (like checking fault
//!   reporting), or to signal that the test is done.
//! - At that point the roles reverse again.
//!
//! # Output
//!
//! Output is produced on ITM stimulus port 8. Output is in a line-oriented
//! human-readable format modeled after report formats like TAP, but avoiding
//! some issues.
//!
//! A test report consists of the following lines:
//!
//! - `meta` - marks the beginning of test suite metainformation
//! - `expect N` - indicates that N (decimal integer) test cases are to follow.
//! - N repeats of:
//!   - `case NAME` - provides the NAME (UTF-8 string not containing newlines) of
//!     the next test case.
//! - `run` - marks the beginning of test suite execution
//! - N repeats of:
//!   - `start NAME` - indicates that test suite NAME (UTF-8 string not
//!     containing newlines) is starting, and any hangs should be blamed on it.
//!   - `finish STATUS NAME` - indicates that test suite NAME has completed with
//!     STATUS (which is `ok` or `FAIL`).
//! - `done STATUS` - signals the end of the test suite. STATUS is `ok` if all
//!   tests passed, `FAIL` if any failed.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use test_api::*;
use userlib::*;
use zerocopy::AsBytes;

#[cfg(armv6m)]
use armv6m_atomic_hack::*;

cfg_if::cfg_if! {
    if #[cfg(armv6m)] {
        /// Helper macro for producing output by semihosting :-(
        macro_rules! test_output {
            ($s:expr) => {
                cortex_m_semihosting::hprintln!($s);
            };
            ($s:expr, $($tt:tt)*) => {
                cortex_m_semihosting::hprintln!($s, $($tt)*);
            };
        }
    } else {
        /// Helper macro for producing output on stimulus port 8.
        macro_rules! test_output {
            ($s:expr) => {
                unsafe {
                    let stim = &mut (*cortex_m::peripheral::ITM::PTR).stim[8];
                    cortex_m::iprintln!(stim, $s);
                }
            };
            ($s:expr, $($tt:tt)*) => {
                unsafe {
                    let stim = &mut (*cortex_m::peripheral::ITM::PTR).stim[8];
                    cortex_m::iprintln!(stim, $s, $($tt)*);
                }
            };
        }
    }
}

/// This runner is written such that the task under test must be task index 1.
/// (And the runner must be zero.)
const TEST_TASK: usize = 1;

#[no_mangle]
static TEST_KICK: AtomicU32 = AtomicU32::new(0);
static TEST_RUNS: AtomicU32 = AtomicU32::new(0);

/// We are sensitive to all notifications, to catch unexpected ones in test.
const ALL_NOTIFICATIONS: u32 = !0;

fn test_run() {
    // Get things rolling by restarting the test task. This ensures that it's
    // running, so that we don't depend on the `start` key in `app.toml` for
    // correctness.
    restart_tester();

    // Begin by interrogating the task to understand the shape of the test
    // suite, and produce the `meta` section.
    test_output!("meta");
    let case_count = get_case_count();
    test_output!("expect {}", case_count);

    // Read and print the name of each test case.
    for i in 0..case_count {
        output_name("case", i);
    }

    // Transition to running tests.
    test_output!("run");
    let mut failures = 0;

    for i in 0..case_count {
        // Restart every time to ensure state is clear.
        restart_tester();

        // Read the name, again. Yes, this means the test suite could change
        // test names on us. Oh well. It's easier than storing the names.
        output_name("start", i);

        // Ask the test to start running. It's *supposed* to immediately reply
        // and then call us back when it finishes.
        start_test(i);

        // We now start playing the receiver, monitoring messages from both the
        // kernel and the testsuite.

        // TODO this is where we need to set a timer, but to do that, we need to
        // be able to read the current time.

        struct MonitorState {
            received_notes: u32,
            test_status: Option<bool>,
        }

        let mut state = MonitorState {
            received_notes: 0,
            test_status: None,
        };

        // Continue monitoring messages until (1) the test has been reported as
        // complete, or (2) we get notice from the kernel that the testsuite has
        // crashed.
        while state.test_status.is_none() {
            hl::recv(
                &mut [],
                ALL_NOTIFICATIONS,
                &mut state,
                |state, bits| {
                    // Record all received notification bits.
                    state.received_notes |= bits;

                    if bits & 1 != 0 {
                        // Uh-oh, somebody faulted.
                        if find_and_report_fault() {
                            // It was the test.
                            state.test_status = Some(false);
                        }
                    }
                },
                |state, op: RunnerOp, msg| -> Result<(), u32> {
                    match op {
                        RunnerOp::ReadAndClearNotes => {
                            let (_, caller) =
                                msg.fixed::<(), u32>().ok_or(2u32)?;
                            caller.reply(state.received_notes);
                            state.received_notes = 0;
                        }
                        RunnerOp::TestComplete => {
                            let (_, caller) =
                                msg.fixed::<(), ()>().ok_or(2u32)?;
                            caller.reply(());
                            state.test_status = Some(true);
                        }
                    }
                    Ok(())
                },
            );
        }

        // Indicate final state of this case.
        let status_str = if state.test_status.unwrap() {
            "finish ok"
        } else {
            failures += 1;
            "finish FAIL"
        };

        output_name(status_str, i);
    }

    // Indicate final state of the suite.
    if failures == 0 {
        test_output!("done pass");
    } else {
        test_output!("done FAIL");
    }
}

#[export_name = "main"]
fn main() -> ! {
    loop {
        test_run();
        TEST_RUNS.fetch_add(1, Ordering::SeqCst);

        while TEST_KICK.load(Ordering::SeqCst) == 0 {
            continue;
        }

        TEST_KICK.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Contacts the test suite to retrieve the name of test case `index`, and then
/// prints it after `context`.
///
/// If the name is invalid UTF-8, substitutes a decimal representation of
/// `index`.
fn output_name(context: &str, index: usize) {
    let mut name = [0; 64];
    let name_slice = get_case_name(index, &mut name);
    if let Ok(name_str) = core::str::from_utf8(name_slice) {
        test_output!("{} {}", context, name_str.trim());
    } else {
        // If any tests are not valid UTF-8, replace their name with their
        // index.
        test_output!("{} {}", context, index);
    }
}

/// Asks the kernel to restart the testsuite task and updates our expected
/// generation.
fn restart_tester() {
    kipc::restart_task(TEST_TASK, true);
}

/// Gets a `TaskId` to the testsuite in its current generation.
fn tester_task_id() -> TaskId {
    sys_refresh_task_id(TaskId::for_index_and_gen(
        TEST_TASK,
        Generation::default(),
    ))
}

/// Contacts the test suite to get the number of defined cases.
fn get_case_count() -> usize {
    let tid = tester_task_id();
    let mut response = 0;
    let op = SuiteOp::GetCaseCount as u16;
    let (rc, len) = sys_send(tid, op, &[], response.as_bytes_mut(), &[]);
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    response
}

/// Contacts the test suite to extract the name of case `id` into `buf`. Returns
/// the prefix of `buf` that contains the returned name (which may be padded
/// with spaces).
fn get_case_name(id: usize, buf: &mut [u8]) -> &[u8] {
    let tid = tester_task_id();
    let op = SuiteOp::GetCaseName as u16;
    let (rc, len) = sys_send(tid, op, &id.as_bytes(), buf, &[]);
    assert_eq!(rc, 0);
    &buf[..len.min(buf.len())]
}

/// Contacts the testsuite to ask to start case `id`.
fn start_test(id: usize) {
    let tid = tester_task_id();
    let op = SuiteOp::RunCase as u16;
    let (rc, len) = sys_send(tid, op, &id.as_bytes(), &mut [], &[]);
    assert_eq!(rc, 0);
    assert_eq!(len, 0);
}

fn log_fault(t: usize, fault: &FaultInfo) {
    match fault {
        FaultInfo::MemoryAccess { address, .. } => match address {
            Some(a) => {
                sys_log!("Task #{} Memory fault at address {:#x}", t, a);
            }

            None => {
                sys_log!("Task #{} Memory fault at unknown address", t);
            }
        },

        FaultInfo::BusError { address, .. } => match address {
            Some(a) => {
                sys_log!("Task #{} Bus error at address {:#x}", t, a);
            }

            None => {
                sys_log!("Task #{} Bus error at unknown address", t);
            }
        },

        FaultInfo::StackOverflow { address, .. } => {
            sys_log!("Task #{} Stack overflow at address {:#x}", t, address);
        }

        FaultInfo::DivideByZero => {
            sys_log!("Task #{} Divide-by-zero", t);
        }

        FaultInfo::IllegalText => {
            sys_log!("Task #{} Illegal text", t);
        }

        FaultInfo::IllegalInstruction => {
            sys_log!("Task #{} Illegal instruction", t);
        }

        FaultInfo::InvalidOperation(details) => {
            sys_log!("Task #{} Invalid operation: {:#010x}", t, details);
        }

        FaultInfo::SyscallUsage(e) => {
            sys_log!("Task #{} Bad Syscall Usage {:?}", t, e);
        }

        FaultInfo::Panic => {
            sys_log!("Task #{} Panic!", t);
        }

        FaultInfo::Injected(who) => {
            sys_log!("Task #{} Fault injected by task #{}", t, who.index());
        }
        FaultInfo::FromServer(who, what) => {
            sys_log!(
                "Task #{} fault from server #{}: {:?}",
                t,
                who.index(),
                what
            );
        }
    }
}

/// Scans the kernel's task table looking for a task that has fallen over.
/// Prints any that are found.
///
/// If the testsuite is found to have fallen over, it is restarted, and this
/// function returns `true`.
fn find_and_report_fault() -> bool {
    let mut tester_faulted = false;
    for i in 0..hubris_num_tasks::NUM_TASKS {
        let s = kipc::read_task_status(i);
        if let TaskState::Faulted { fault, .. } = s {
            log_fault(i, &fault);
            if i == TEST_TASK {
                tester_faulted = true;
                restart_tester();
            }
        }
    }
    tester_faulted
}
