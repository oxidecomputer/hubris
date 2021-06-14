//! External interface for Jefe
//!
//! It can be very handy to have Jefe be externally influenced by a debugger.
//! This allows tasks to be remotely controlled at some level:  they can
//! be held on a fault, started (when stopped), etc.  The interface for this
//! relies on variables at well-known locations; the debugger knows to find
//! these locations and modify them.
//!

use crate::Disposition;
use core::sync::atomic::{AtomicU32, Ordering};

use ringbuf::*;
use userlib::*;

/// The actual requests that we honor from an external source entity
#[derive(FromPrimitive, Copy, Clone, Debug, PartialEq)]
enum Request {
    None = 0,
    Start = 1,
    Hold = 2,
    Release = 3,
    Fault = 4,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Error {
    IllegalTask,
    BadTask,
    BadRequest,
}

#[derive(Copy, Clone, Debug, PartialEq)]
struct TaskIndex(u16);

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    Request(Request, TaskIndex),
    Disposition(TaskIndex, Disposition),
    Error(Error),
}

ringbuf!(Trace, 4, Trace::None);

static JEFE_EXTERNAL_READY: AtomicU32 = AtomicU32::new(0);
static JEFE_EXTERNAL_REQUEST: AtomicU32 = AtomicU32::new(0);
static JEFE_EXTERNAL_TASKINDEX: AtomicU32 = AtomicU32::new(0);
static JEFE_EXTERNAL_KICK: AtomicU32 = AtomicU32::new(0);
static JEFE_EXTERNAL_REQUESTS: AtomicU32 = AtomicU32::new(0);
static JEFE_EXTERNAL_ERRORS: AtomicU32 = AtomicU32::new(0);

///
/// Checks for any external requests for change in task disposition,
/// potentially modifying the passed array.  Returns a boolean to indicate if
/// a valid external request was received.
///
pub fn check(disposition: &mut [Disposition]) -> bool {
    if JEFE_EXTERNAL_KICK.swap(0, Ordering::SeqCst) == 0 {
        return false;
    }

    let val = JEFE_EXTERNAL_REQUEST.load(Ordering::SeqCst);

    if let Some(request) = Request::from_u32(val) {
        let ndx = JEFE_EXTERNAL_TASKINDEX.load(Ordering::SeqCst) as usize;

        if ndx == 0 {
            ringbuf_entry!(Trace::Error(Error::IllegalTask));
        } else if ndx >= disposition.len() {
            ringbuf_entry!(Trace::Error(Error::BadTask));
        } else {
            let task = TaskIndex(ndx as u16);
            ringbuf_entry!(Trace::Request(request, task));

            disposition[ndx] = match request {
                Request::None => disposition[ndx],
                Request::Hold => Disposition::Hold,
                Request::Release => Disposition::Restart,
                Request::Start => Disposition::Start,
                Request::Fault => Disposition::Fault,
            };

            ringbuf_entry!(Trace::Disposition(task, disposition[ndx]));
            JEFE_EXTERNAL_REQUESTS.fetch_add(1, Ordering::SeqCst);

            return true;
        }
    } else {
        ringbuf_entry!(Trace::Error(Error::BadRequest));
    }

    JEFE_EXTERNAL_ERRORS.fetch_add(1, Ordering::SeqCst);
    return false;
}

///
/// Indicates that we are ready for external control.
///
pub fn set_ready() {
    JEFE_EXTERNAL_READY.fetch_add(1, Ordering::SeqCst);
}
