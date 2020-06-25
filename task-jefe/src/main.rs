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

const NUM_OTHER_TASKS: usize = NUM_TASKS - 1;

#[export_name = "main"]
fn main() -> ! {
    sys_log!("viva el jefe");

    // Set up our async message descriptor table for sending heartbeats. We're
    // setting up a message to every task but ourselves, because checking
    // ourselves would be weird.
    let async_table = get_async_descriptor_table();
    for (i, desc) in async_table.iter_mut().enumerate() {
        let task_id = TaskId::for_index_and_gen(i + 1, Generation::default());
        configure_heartbeat(desc, task_id);
    }

    // Set up our notification mask.
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
                            Some(a) => {
                                sys_log!("Task #{} Memory fault at address 0x{:x}", i, a);
                            }

                            None => sys_log!("Task #{} Memory fault at unknown address", i)
                        }
                        abi::FaultInfo::SyscallUsage(e) =>
                                sys_log!("Task #{} Bad Syscall Usage {:?}", i, e),
                        abi::FaultInfo::Panic => sys_log!("Task #{} Panic!", i),
                    };
                    // Stand it back up.
                    kipc::restart_task(i, true);
                }
            }
        } else {
            // ...huh. A task has sent a message to us. That seems wrong.
            sys_log!("Unexpected message from {}", msginfo.sender.0);
        }
    }
}

fn get_async_descriptor_table() -> &'static mut [AsyncDesc; NUM_OTHER_TASKS] {
    // Bring in some names that we don't use anywhere else.
    use core::mem::MaybeUninit;
    use core::sync::atomic::{AtomicBool, Ordering};

    // A global atomic flag defends us from aliasing. Each pass through this
    // function swaps its value for true; only one caller ever gets false in
    // response.
    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::SeqCst) {
        panic!();
    }

    // Scoping the static mut like this ensures that no other code path anywhere
    // in the program can directly reference it, even using unsafe. Combined
    // with the check above this ensures that the access is truly unique. This
    // also provides the safety proof for our unsafe block: static mut is unsafe
    // because of aliasing and races, but we're avoiding both.
    let uninit_table = unsafe {
        static mut TABLE: [MaybeUninit<AsyncDesc>; NUM_OTHER_TASKS] = [MaybeUninit::uninit(); NUM_OTHER_TASKS];
        &mut TABLE
    };

    // Initialize each element in the table. Use a pointer write to overwrite
    // the uninitialized storage without interpreting its former contents.
    for desc in uninit_table.iter_mut() {
        unsafe {
            desc.as_mut_ptr().write(AsyncDesc::empty());
        }
    }

    // Having initialized them, we can reinterpret it as AsyncDescs.
    // Safety: we have initialized each element just above.
    unsafe {
        core::mem::transmute::<_, &mut [AsyncDesc; NUM_OTHER_TASKS]>(uninit_table)
    }
}

fn configure_heartbeat(
    desc: &mut AsyncDesc,
    dest: TaskId,
) {
    const HEARTBEAT: u16 = 0xFFFF;

    // Because we have a &mut, we know the kernel is not currently monitoring
    // this descriptor -- thus we can rewrite it willy-nilly without worry of
    // races.

    desc.operation = HEARTBEAT;
    desc.dest = dest;
    desc.length = 0;
    desc.on_deliver = 0;

    desc.set_state(AsyncState::Pending);
}
