// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implementation of the system supervisor.
//!
//! The supervisor is responsible for:
//!
//! - Maintaining the system console output (currently via semihosting).
//! - Monitoring tasks for failures and restarting them.
//!
//! It will probably become responsible for:
//!
//! - Evacuating kernel log information.
//! - Coordinating certain shared resources, such as the RCC and GPIO muxing.
//! - Managing a watchdog timer.
//!
//! It's unwise for the supervisor to use `SEND`, ever, except to talk to the
//! kernel. This is because a `SEND` to a misbehaving task could block forever,
//! taking out the supervisor. The long-term idea is to provide some sort of
//! asynchronous messaging from the supervisor to less-trusted tasks, but that
//! doesn't exist yet, so we're mostly using RECV/REPLY and notifications. This
//! means that hardware drivers required for this task must be built in instead
//! of running in separate tasks.

#![no_std]
#![no_main]

mod external;

use userlib::*;

fn log_fault(t: usize, fault: &abi::FaultInfo) {
    match fault {
        abi::FaultInfo::MemoryAccess { address, .. } => match address {
            Some(a) => {
                sys_log!("Task #{} Memory fault at address {:#x}", t, a);
            }

            None => {
                sys_log!("Task #{} Memory fault at unknown address", t);
            }
        },

        abi::FaultInfo::BusError { address, .. } => match address {
            Some(a) => {
                sys_log!("Task #{} Bus error at address {:#x}", t, a);
            }

            None => {
                sys_log!("Task #{} Bus error at unknown address", t);
            }
        },

        abi::FaultInfo::StackOverflow { address, .. } => {
            sys_log!("Task #{} Stack overflow at address {:#x}", t, address);
        }

        abi::FaultInfo::DivideByZero => {
            sys_log!("Task #{} Divide-by-zero", t);
        }

        abi::FaultInfo::IllegalText => {
            sys_log!("Task #{} Illegal text", t);
        }

        abi::FaultInfo::IllegalInstruction => {
            sys_log!("Task #{} Illegal instruction", t);
        }

        abi::FaultInfo::InvalidOperation(details) => {
            sys_log!("Task #{} Invalid operation: {:#010x}", t, details);
        }

        abi::FaultInfo::SyscallUsage(e) => {
            sys_log!("Task #{} Bad Syscall Usage {:?}", t, e);
        }

        abi::FaultInfo::Panic => {
            sys_log!("Task #{} Panic!", t);
        }

        abi::FaultInfo::Injected(who) => {
            sys_log!("Task #{} Fault injected by task #{}", t, who.index());
        }
        abi::FaultInfo::FromServer(who, what) => {
            sys_log!(
                "Task #{} Fault from server #{}: {:?}",
                t,
                who.index(),
                what
            );
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Disposition {
    Restart,
    Start,
    Hold,
    Fault,
}

#[export_name = "main"]
fn main() -> ! {
    sys_log!("viva el jefe");

    let mut disposition: [Disposition; hubris_num_tasks::NUM_TASKS] =
        [Disposition::Restart; hubris_num_tasks::NUM_TASKS];
    let mut logged: [bool; hubris_num_tasks::NUM_TASKS] =
        [false; hubris_num_tasks::NUM_TASKS];

    // We'll have notification 0 wired up to receive information about task
    // faults.
    let fault_mask = 1;

    // We install a timeout to periodcally check for an external direction
    // of our task disposition (e.g., via Humility).  This timeout should
    // generally be fast for a human but slow for a computer; we pick a
    // value of ~100 ms.  Our timer mask can't conflict with our fault
    // notification, but can otherwise be arbitrary.
    const TIMER_MASK: u32 = 1 << 1;
    const TIMER_INTERVAL: u64 = 100;
    let mut deadline = TIMER_INTERVAL;

    sys_set_timer(Some(deadline), TIMER_MASK);

    external::set_ready();

    loop {
        let msginfo = sys_recv_open(&mut [], fault_mask | TIMER_MASK);

        if msginfo.sender == TaskId::KERNEL {
            // Check to see if we have any external requests
            let changed = external::check(&mut disposition);

            // If our timer went off, we need to reestablish it
            if msginfo.operation & TIMER_MASK != 0 {
                deadline += TIMER_INTERVAL;
                sys_set_timer(Some(deadline), TIMER_MASK);
            }

            // If our disposition has changed or if we have been notified of
            // a faulting task, we need to iterate over all of our tasks.
            if changed || (msginfo.operation & fault_mask) != 0 {
                for i in 0..hubris_num_tasks::NUM_TASKS {
                    match kipc::read_task_status(i) {
                        abi::TaskState::Faulted { fault, .. } => {
                            if !logged[i] {
                                log_fault(i, &fault);
                                logged[i] = true;
                            }

                            if disposition[i] == Disposition::Restart {
                                // Stand it back up
                                kipc::restart_task(i, true);
                                logged[i] = false;
                            }
                        }

                        abi::TaskState::Healthy(abi::SchedState::Stopped) => {
                            if disposition[i] == Disposition::Start {
                                kipc::restart_task(i, true);
                            }
                        }

                        abi::TaskState::Healthy(..) => {
                            if disposition[i] == Disposition::Fault {
                                kipc::fault_task(i);
                            }
                        }
                    }
                }
            }
        } else {
            // ...huh. A task has sent a message to us. That seems wrong.
            sys_log!("Unexpected message from {}", msginfo.sender.0);
        }
    }
}
