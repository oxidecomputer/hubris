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

use userlib::*;
use cortex_m_semihosting::hprintln;

#[export_name = "main"]
fn main() -> ! {
    hprintln!("viva el jefe").ok();

    // We'll have notification 0 wired up to receive information about task
    // faults.
    let mask = 1;
    loop {
        let msginfo = sys_recv(&mut [], mask);

        if msginfo.sender == TaskId::KERNEL {
            // Handle notification
            // We'll assume this notification represents a fault, since we only
            // had the one bit enabled in the mask... which task has fallen
            // over?
            for i in 0..NUM_TASKS {
                let s = kipc::read_task_status(i);
                if let abi::TaskState::Faulted { fault, .. } = s {
                    match fault {
                        abi::FaultInfo::MemoryAccess { address, .. } =>
                        match address {
                            Some(a) =>  hprintln!("Task #{} Memory fault at address 0x{:x}", i, a).ok(),
                            None => hprintln!("Task #{} Memory fault at unknown address", i).ok()
                        }
                        abi::FaultInfo::SyscallUsage(e) =>
                                hprintln!("Task #{} Bad Syscall Usage {:?}", i, e).ok(),
                        abi::FaultInfo::Panic => hprintln!("Task #{} Panic!", i).ok(),
                    };
                    // Stand it back up.
                    kipc::restart_task(i, true);
                }
            }
        } else {
            // ...huh. A task has sent a message to us. That seems wrong.
            hprintln!("Unexpected message from {}", msginfo.sender.0).ok();
        }
    }
}
