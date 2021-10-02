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

use ringbuf::*;
use task_jefe_api::*;
use userlib::*;

fn log_fault(t: usize, fault: &abi::FaultInfo) {
    match fault {
        abi::FaultInfo::MemoryAccess { address, .. } => match address {
            Some(a) => {
                sys_log!("Task #{} Memory fault at address 0x{:x}", t, a);
            }

            None => {
                sys_log!("Task #{} Memory fault at unknown address", t);
            }
        },

        abi::FaultInfo::BusError { address, .. } => match address {
            Some(a) => {
                sys_log!("Task #{} Bus error at address 0x{:x}", t, a);
            }

            None => {
                sys_log!("Task #{} Bus error at unknown address", t);
            }
        },

        abi::FaultInfo::StackOverflow { address, .. } => {
            sys_log!("Task #{} Stack overflow at address 0x{:x}", t, address);
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
            sys_log!("Task #{} Invalid operation: 0x{:08x}", t, details);
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
    }
}

//
// Iterate over tasks to see if any have faulted, need to have a fault
// injected, or need to be restarted.
//
fn check_tasks(
    disposition: &[Disposition; NUM_TASKS],
    logged: &mut [bool; NUM_TASKS],
) {
    for i in 0..NUM_TASKS {
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

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    SetDisposition(u16, Disposition),
    None,
}

ringbuf!(Trace, 32, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    sys_log!("viva el jefe");

    let disposition: [Disposition; NUM_TASKS] =
        [Disposition::Restart; NUM_TASKS];
    let logged: [bool; NUM_TASKS] = [false; NUM_TASKS];

    // We'll have notification 0 wired up to receive information about task
    // faults.
    let fault_mask = 1;
    let mut state = (disposition, logged);
    let mut buffer = [0; 4];

    loop {
        hl::recv(
            &mut buffer,
            fault_mask,
            &mut state,
            |(ref disposition, ref mut logged), bits| {
                if (bits & fault_mask) != 0 {
                    check_tasks(disposition, logged)
                }
            },
            |state, op: Op, msg| -> Result<(), JefeError> {
                match op {
                    Op::SetDisposition => {
                        let (msg, caller) = msg
                            .fixed::<SetDispositionRequest, ()>()
                            .ok_or(JefeError::BadArg)?;

                        let task = msg.task;
                        let disposition = Disposition::from_u8(msg.disposition)
                            .ok_or(JefeError::BadArg)?;

                        ringbuf_entry!(Trace::SetDisposition(
                            task,
                            disposition
                        ));

                        if task == 0 {
                            return Err(JefeError::IllegalTask);
                        }

                        let ndx = task as usize;

                        if ndx >= state.0.len() {
                            return Err(JefeError::BadTask);
                        }

                        state.0[ndx] = disposition;
                        check_tasks(&state.0, &mut state.1);

                        caller.reply(());
                        Ok(())
                    }
                }
            },
        );
    }
}
