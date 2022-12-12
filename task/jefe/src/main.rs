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

use core::convert::Infallible;

use hubris_num_tasks::NUM_TASKS;
use task_jefe_api::ResetReason;
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Disposition {
    Restart,
    Start,
    Hold,
    Fault,
}

// We install a timeout to periodcally check for an external direction
// of our task disposition (e.g., via Humility).  This timeout should
// generally be fast for a human but slow for a computer; we pick a
// value of ~100 ms.  Our timer mask can't conflict with our fault
// notification, but can otherwise be arbitrary.
const TIMER_INTERVAL: u64 = 100;
const TIMER_MASK: u32 = 1 << 1;
// We'll have notification 0 wired up to receive information about task faults.
const FAULT_MASK: u32 = 1 << 0;

#[export_name = "main"]
fn main() -> ! {
    sys_log!("viva el jefe");

    let mut disposition: [Disposition; hubris_num_tasks::NUM_TASKS] =
        [Disposition::Restart; hubris_num_tasks::NUM_TASKS];

    for held_task in generated::HELD_TASKS {
        disposition[held_task as usize] = Disposition::Hold;
    }

    let mut logged: [bool; hubris_num_tasks::NUM_TASKS] =
        [false; hubris_num_tasks::NUM_TASKS];
    let deadline = sys_get_timer().now + TIMER_INTERVAL;

    sys_set_timer(Some(deadline), TIMER_MASK);

    external::set_ready();

    let mut server = ServerImpl {
        state: 0,
        deadline,
        disposition: &mut disposition,
        logged: &mut logged,
        reset_reason: ResetReason::Unknown,
    };
    let mut buf = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch_n(&mut buf, &mut server);
    }
}

struct ServerImpl<'s> {
    state: u32,
    disposition: &'s mut [Disposition; NUM_TASKS],
    logged: &'s mut [bool; NUM_TASKS],
    deadline: u64,
    reset_reason: ResetReason,
}

impl idl::InOrderJefeImpl for ServerImpl<'_> {
    fn request_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        // If we wanted to broadcast to other tasks that a restart is occuring
        // here is where we would do so!
        kipc::system_restart();
    }

    fn get_reset_reason(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ResetReason, idol_runtime::RequestError<Infallible>> {
        Ok(self.reset_reason)
    }

    fn set_reset_reason(
        &mut self,
        _msg: &userlib::RecvMessage,
        reason: ResetReason,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        self.reset_reason = reason;
        Ok(())
    }

    fn get_state(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<u32, idol_runtime::RequestError<Infallible>> {
        Ok(self.state)
    }

    fn set_state(
        &mut self,
        _msg: &userlib::RecvMessage,
        state: u32,
    ) -> Result<(), idol_runtime::RequestError<Infallible>> {
        if self.state != state {
            self.state = state;

            for (task, mask) in generated::MAILING_LIST {
                let taskid =
                    TaskId::for_index_and_gen(task as usize, Generation::ZERO);
                let taskid = sys_refresh_task_id(taskid);
                sys_post(taskid, mask);
            }
        }
        Ok(())
    }
}

impl idol_runtime::NotificationHandler for ServerImpl<'_> {
    fn current_notification_mask(&self) -> u32 {
        FAULT_MASK | TIMER_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        // Check to see if we have any external requests
        let changed = external::check(self.disposition);

        // If our timer went off, we need to reestablish it
        if bits & TIMER_MASK != 0 {
            self.deadline += TIMER_INTERVAL;
            sys_set_timer(Some(self.deadline), TIMER_MASK);
        }

        // If our disposition has changed or if we have been notified of
        // a faulting task, we need to iterate over all of our tasks.
        if changed || (bits & FAULT_MASK) != 0 {
            for i in 0..NUM_TASKS {
                match kipc::read_task_status(i) {
                    abi::TaskState::Faulted { fault, .. } => {
                        if !self.logged[i] {
                            log_fault(i, &fault);
                            self.logged[i] = true;
                        }

                        if self.disposition[i] == Disposition::Restart {
                            // Stand it back up
                            kipc::restart_task(i, true);
                            self.logged[i] = false;
                        }
                    }

                    abi::TaskState::Healthy(abi::SchedState::Stopped) => {
                        if self.disposition[i] == Disposition::Start {
                            kipc::restart_task(i, true);
                        }
                    }

                    abi::TaskState::Healthy(..) => {
                        if self.disposition[i] == Disposition::Fault {
                            kipc::fault_task(i);
                        }
                    }
                }
            }
        }
    }
}

// Place to namespace all the bits generated by our config processor.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/jefe_config.rs"));
}

// And the Idol bits
mod idl {
    use task_jefe_api::ResetReason;
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
