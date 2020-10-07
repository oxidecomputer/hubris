use crate::{sys_send, Lease};

use abi::{Generation, TaskId};

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

#[defmt::global_logger]
struct Logger;

static mut BUFFER: [u8; 256] = [0; 256];
static mut BUFFER_LEN: usize = 0;

static ACQUIRED: AtomicBool = AtomicBool::new(false);

impl defmt::Write for Logger {
    fn write(&mut self, bytes: &[u8]) {
        unsafe {
            let last_pos = bytes.len() + BUFFER_LEN;
            BUFFER[BUFFER_LEN..last_pos].copy_from_slice(bytes);
            BUFFER_LEN += bytes.len();
        }
    }
}

unsafe impl defmt::Logger for Logger {
    fn acquire() -> Option<NonNull<dyn defmt::Write>> {
        if ACQUIRED.swap(true, Ordering::SeqCst) {
            panic!();
        }

        Some(NonNull::from(&Logger as &dyn defmt::Write))
    }

    unsafe fn release(_: NonNull<dyn defmt::Write>) {
        ACQUIRED.store(false, Ordering::SeqCst);

        send_to_log_task();

        BUFFER_LEN = 0;
    }
}

// We want to send stuff to the log task, so if we're not in standalone
// mode, let's do that.
#[cfg(not(standalone))]
fn send_to_log_task() {
    extern "C" {
        static log_task_id: u16;
    }

    unsafe {
        let log_id = *(&log_task_id as *const u16);

        let log =
            TaskId::for_index_and_gen(log_id as usize, Generation::default());
        sys_send(log, 1, &[], &mut [], &[Lease::from(&BUFFER[0..BUFFER_LEN])]);
    }
}

// If we are standalone, we don't know what the log task is, so let's not
// try and do that.
#[cfg(standalone)]
fn send_to_log_task() {}
