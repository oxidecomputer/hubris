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
#![forbid(clippy::wildcard_imports)]

#[cfg(feature = "dump")]
mod dump;

mod external;

use core::convert::Infallible;

use hubris_num_tasks::NUM_TASKS;
use humpty::DumpArea;
use idol_runtime::RequestError;
use task_jefe_api::{DumpAgentError, ResetReason};
use userlib::{kipc, Generation, TaskId};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
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
const TIMER_INTERVAL: u32 = 100;

#[export_name = "main"]
fn main() -> ! {
    let mut task_states = [TaskStatus::default(); hubris_num_tasks::NUM_TASKS];
    for held_task in generated::HELD_TASKS {
        task_states[held_task as usize].disposition = Disposition::Hold;
    }

    let deadline =
        userlib::set_timer_relative(TIMER_INTERVAL, notifications::TIMER_MASK);

    external::set_ready();

    let mut server = ServerImpl {
        state: 0,
        deadline,
        task_states: &mut task_states,
        reset_reason: ResetReason::Unknown,

        #[cfg(feature = "dump")]
        dump_areas: dump::initialize_dump_areas(),

        #[cfg(feature = "dump")]
        last_dump_area: None,
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

    /// Base address for a linked list of dump areas
    #[cfg(feature = "dump")]
    dump_areas: u32,

    /// Cache of most recently checked dump area
    ///
    /// This accelerates our linked-list search in the common case of doing
    /// sequential reads through dump memory.
    #[cfg(feature = "dump")]
    last_dump_area: Option<DumpArea>,
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
                let taskid = userlib::sys_refresh_task_id(taskid);
                userlib::sys_post(taskid, mask);
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
                // If we have cached a dump area, then use it to accelerate
                // lookup by jumping partway through the linked list
                let d = if let Some(prev) = self.last_dump_area {
                    // We are after (or exactly at) our previously cached dump
                    // area.  The start address should be the same, so we don't
                    // need to walk to it, but we'll reload from from memory in
                    // case other data in the header has changed.
                    if let Some(offset) = index.checked_sub(prev.index) {
                        let mut d =
                            dump::get_dump_area(prev.region.address, offset);
                        if let Ok(d) = &mut d {
                            d.index += prev.index;
                        }
                        d
                    } else {
                        // Default case: we have to search from the start
                        dump::get_dump_area(self.dump_areas, index)
                    }
                } else {
                    dump::get_dump_area(self.dump_areas, index)
                };
                let d = d.map_err(RequestError::from)?;
                self.last_dump_area = Some(d);
                Ok(d)
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
            if userlib::sys_get_timer().now >= self.deadline {
                self.deadline = userlib::set_timer_relative(
                    TIMER_INTERVAL,
                    notifications::TIMER_MASK,
                );
            }
        }

        if bits & notifications::FAULT_MASK != 0 {
            // Work out who faulted. It's theoretically possible for more than
            // one task to have faulted since we last looked, but it's somewhat
            // unlikely since a fault causes us to immediately preempt. In any
            // case, let's assume we might have to handle multiple tasks.
            let mut next_task = 1;
            while let Some(fault_index) = kipc::find_faulted_task(next_task) {
                let fault_index = usize::from(fault_index);
                // This addition cannot overflow in practice, because the number
                // of tasks in the system is very much smaller than 2**32. So we
                // use wrapping add, because currently the compiler doesn't
                // understand this property.
                next_task = fault_index.wrapping_add(1);

                // Safety: `fault_index` is from the kernel, and the kernel will
                // not give us an out-of-range task index.
                //
                // TODO: it might be nice to fold this into a utility function
                // in kipc or something
                let status =
                    unsafe { self.task_states.get_unchecked_mut(fault_index) };

                // If we're aware that this task is in a fault state, don't
                // bother making a syscall to enquire.
                if status.holding_fault {
                    continue;
                }

                #[cfg(feature = "dump")]
                {
                    // We'll ignore the result of dumping; it could fail
                    // if we're out of space, but we don't have a way of
                    // dealing with that right now.
                    //
                    // TODO: some kind of circular buffer?
                    _ = dump::dump_task(self.dump_areas, fault_index);
                }

                if status.disposition == Disposition::Restart {
                    // Stand it back up
                    kipc::restart_task(fault_index, true);
                } else {
                    // Mark this one off so we don't revisit it until
                    // requested.
                    status.holding_fault = true;
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
