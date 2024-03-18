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

#[cfg(feature = "dump")]
mod dump;

mod external;

use core::convert::Infallible;

use hubris_num_tasks::NUM_TASKS;
use humpty::DumpArea;
use idol_runtime::RequestError;
use task_jefe_api::{DumpAgentError, ResetReason};
use userlib::*;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Default, counters::Count)]
pub enum Disposition {
    #[default]
    Restart,
    Hold,
}

// We install a timeout to periodically check for an external direction
// of our task disposition (e.g., via Humility).  This timeout should
// generally be fast for a human but slow for a computer; we pick a
// value of ~100 ms.  Our timer mask can't conflict with our fault
// notification, but can otherwise be arbitrary.
const TIMER_INTERVAL: u64 = 100;

#[export_name = "main"]
fn main() -> ! {
    let mut task_states = [TaskStatus::default(); hubris_num_tasks::NUM_TASKS];
    for held_task in generated::HELD_TASKS {
        task_states[held_task as usize].disposition = Disposition::Hold;
    }

    let deadline = sys_get_timer().now + TIMER_INTERVAL;

    sys_set_timer(Some(deadline), notifications::TIMER_MASK);

    external::set_ready();

    let mut server = ServerImpl {
        state: 0,
        deadline,
        task_states: &mut task_states,
        reset_reason: ResetReason::Unknown,
        #[cfg(feature = "dump")]
        dump_areas: dump::initialize_dump_areas(),
    };
    let mut buf = [0u8; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buf, &mut server);
    }
}

struct ServerImpl<'s> {
    state: u32,
    task_states: &'s mut [TaskStatus; NUM_TASKS],
    deadline: u64,
    reset_reason: ResetReason,
    #[cfg(feature = "dump")]
    dump_areas: u32,
}

impl idl::InOrderJefeImpl for ServerImpl<'_> {
    fn request_reset(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError<Infallible>> {
        // If we wanted to broadcast to other tasks that a restart is occuring
        // here is where we would do so!
        kipc::system_restart();
    }

    fn get_reset_reason(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<ResetReason, RequestError<Infallible>> {
        Ok(self.reset_reason)
    }

    fn set_reset_reason(
        &mut self,
        _msg: &userlib::RecvMessage,
        reason: ResetReason,
    ) -> Result<(), RequestError<Infallible>> {
        self.reset_reason = reason;
        Ok(())
    }

    fn get_state(
        &mut self,
        _msg: &userlib::RecvMessage,
    ) -> Result<u32, RequestError<Infallible>> {
        Ok(self.state)
    }

    fn set_state(
        &mut self,
        _msg: &userlib::RecvMessage,
        state: u32,
    ) -> Result<(), RequestError<Infallible>> {
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

    fn restart_me_raw(
        &mut self,
        msg: &userlib::RecvMessage,
    ) -> Result<(), RequestError<Infallible>> {
        kipc::restart_task(msg.sender.index(), true);

        // Note: the returned value here won't go anywhere because we just
        // unblocked the caller. So this is doing a small amount of unnecessary
        // work. This is a compromise because Idol can't easily describe an IPC
        // that won't return at this time.
        Ok(())
    }

    cfg_if::cfg_if! {
        if #[cfg(feature = "dump")] {
            fn get_dump_area(
                &mut self,
                _msg: &userlib::RecvMessage,
                index: u8,
            ) -> Result<DumpArea, RequestError<DumpAgentError>> {
                dump::get_dump_area(self.dump_areas, index)
                    .map_err(|e| e.into())
            }

            fn claim_dump_area(
                &mut self,
                _msg: &userlib::RecvMessage,
            ) -> Result<DumpArea, RequestError<DumpAgentError>> {
                dump::claim_dump_area(self.dump_areas).map_err(|e| e.into())
            }

            fn reinitialize_dump_areas(
                &mut self,
                _msg: &userlib::RecvMessage,
            ) -> Result<(), RequestError<DumpAgentError>> {
                self.dump_areas = dump::initialize_dump_areas();
                Ok(())
            }

            fn dump_task(
                &mut self,
                _msg: &userlib::RecvMessage,
                task_index: u32,
            ) -> Result<u8, RequestError<DumpAgentError>> {
                // `dump::dump_task` doesn't check the task index, because it's
                // normally called by a trusted source; we'll do it ourself.
                if task_index == 0 {
                    // Can't dump the supervisor
                    return Err(DumpAgentError::NotSupported.into());
                } else if task_index as usize >= self.task_states.len() {
                    // Can't dump a non-existent task
                    return Err(DumpAgentError::BadOffset.into());
                }
                dump::dump_task(self.dump_areas, task_index as usize)
                    .map_err(|e| e.into())
            }

            fn dump_task_region(
                &mut self,
                _msg: &userlib::RecvMessage,
                task_index: u32,
                address: u32,
                length: u32,
            ) -> Result<u8, RequestError<DumpAgentError>> {
                if task_index == 0 {
                    return Err(DumpAgentError::NotSupported.into());
                } else if task_index as usize >= self.task_states.len() {
                    return Err(DumpAgentError::BadOffset.into());
                }
                dump::dump_task_region(
                    self.dump_areas, task_index as usize, address, length
                ).map_err(|e| e.into())
            }

            fn reinitialize_dump_from(
                &mut self,
                _msg: &userlib::RecvMessage,
                index: u8,
            ) -> Result<(), RequestError<DumpAgentError>> {
                dump::reinitialize_dump_from(self.dump_areas, index)
                    .map_err(|e| e.into())
            }
        } else {
            fn get_dump_area(
                &mut self,
                _msg: &userlib::RecvMessage,
                _index: u8,
            ) -> Result<DumpArea, RequestError<DumpAgentError>> {
                Err(DumpAgentError::DumpAgentUnsupported.into())
            }

            fn claim_dump_area(
                &mut self,
                _msg: &userlib::RecvMessage,
            ) -> Result<DumpArea, RequestError<DumpAgentError>> {
                Err(DumpAgentError::DumpAgentUnsupported.into())
            }

            fn reinitialize_dump_areas(
                &mut self,
                _msg: &userlib::RecvMessage,
            ) -> Result<(), RequestError<DumpAgentError>> {
                Err(DumpAgentError::DumpAgentUnsupported.into())
            }

            fn dump_task(
                &mut self,
                _msg: &userlib::RecvMessage,
                _task_index: u32,
            ) -> Result<u8, RequestError<DumpAgentError>> {
                Err(DumpAgentError::DumpAgentUnsupported.into())
            }

            fn dump_task_region(
                &mut self,
                _msg: &userlib::RecvMessage,
                _task_index: u32,
                _address: u32,
                _length: u32,
            ) -> Result<u8, RequestError<DumpAgentError>> {
                Err(DumpAgentError::DumpAgentUnsupported.into())
            }

            fn reinitialize_dump_from(
                &mut self,
                _msg: &userlib::RecvMessage,
                _index: u8,
            ) -> Result<(), RequestError<DumpAgentError>> {
                Err(DumpAgentError::DumpAgentUnsupported.into())
            }
        }
    }
}

/// Structure we use for tracking the state of the tasks we supervise. There is
/// one of these per supervised task.
#[derive(Copy, Clone, Debug, Default)]
struct TaskStatus {
    disposition: Disposition,
    holding_fault: bool,
}

impl idol_runtime::NotificationHandler for ServerImpl<'_> {
    fn current_notification_mask(&self) -> u32 {
        notifications::FAULT_MASK | notifications::TIMER_MASK
    }

    fn handle_notification(&mut self, bits: u32) {
        // Handle any external (debugger) requests.
        external::check(self.task_states);

        if bits & notifications::TIMER_MASK != 0 {
            // If our timer went off, we need to reestablish it
            if sys_get_timer().now >= self.deadline {
                self.deadline += TIMER_INTERVAL;
                sys_set_timer(Some(self.deadline), notifications::TIMER_MASK);
            }
        }

        if bits & notifications::FAULT_MASK != 0 {
            // Work out who faulted. It's theoretically possible for more than
            // one task to have faulted since we last looked, but it's somewhat
            // unlikely since a fault causes us to immediately preempt. In any
            // case, let's assume we might have to handle multiple tasks.
            //
            // TODO: it would be fantastic to have a way of finding this out in
            // one syscall.
            for (i, status) in self.task_states.iter_mut().enumerate() {
                // If we're aware that this task is in a fault state, don't
                // bother making a syscall to enquire.
                if status.holding_fault {
                    continue;
                }

                if let abi::TaskState::Faulted { .. } =
                    kipc::read_task_status(i)
                {
                    #[cfg(feature = "dump")]
                    {
                        // We'll ignore the result of dumping; it could fail
                        // if we're out of space, but we don't have a way of
                        // dealing with that right now.
                        //
                        // TODO: some kind of circular buffer?
                        _ = dump::dump_task(self.dump_areas, i);
                    }

                    if status.disposition == Disposition::Restart {
                        // Stand it back up
                        kipc::restart_task(i, true);
                    } else {
                        // Mark this one off so we don't revisit it until
                        // requested.
                        status.holding_fault = true;
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

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

// And the Idol bits
mod idl {
    use task_jefe_api::{DumpAgentError, ResetReason};
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
