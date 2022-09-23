// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! External interface for Jefe
//!
//! It can be very handy to have Jefe be externally influenced by a debugger.
//! This allows tasks to be remotely controlled at some level:  they can be
//! held on a fault, started (when stopped), etc.  The interface for this
//! relies on variables at well-known locations; the debugger knows to find
//! these locations and modify them.
//!
//! But wait, you might well exclaim: doesn't Hubris already have an external
//! debugger interface in HIF that could be used for this?  And wouldn't it
//! really be much more elegant to have Jefe have an interface to set task
//! restart disposition, and then have Humility use its HIF mechanisms to call
//! it?  Indeed, this is so tantalizing that this interface was in fact
//! reimplemented exactly that way -- only to discover some unintended
//! consequences.  First, the task that executes HIF needs to be the lowest
//! priority task in the system to allow it to call other, high priority tasks
//! (thereby avoiding inversion) -- but by dint of being the lowest priority,
//! it can be starved by essentially every other task.  This becomes
//! problematic when a high priority task is in a fault loop, as it becomes
//! impossible for the HIF execution engine to actually get scheduled to
//! tell Jefe to stop restarting the faulting task.  This is frustrating for
//! the user:  they are (correctly) trying to tell Jefe to hold the faulting
//! task, and they will be greeted with nothing but execution timeouts.
//!
//! And if that weren't enough, having Jefe's restart disposition set via HIF
//! execution introduces another thorny problem: if the HIF execution is
//! itself inducing a panic in a server that it is calling (as it may well in
//! development), it is natural to want to hold that task on the fault.  Once
//! the task is held, the HIF execution engine will be held too (as it is its
//! call that is inducing the fault).  There is nothing wrong with that --
//! until it comes time to unhold the task.  Under these conditions, the
//! system is wedged:  the HIF task cannot execute (it is reply-blocked) to
//! tell Jefe to restart the faulted task.
//!
//! These problems were a clear message from the gods: we were being punished
//! for the hubris of a meaningless elegance.  Seeing the folly of our mortal
//! ways, we restored the logic you see before you -- but added this
//! additional warning, surely fated to become half sunk in the lone and level
//! sands...
//!

use crate::Disposition;
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(armv6m)]
use armv6m_atomic_hack::AtomicU32Ext;

use ringbuf::*;
use userlib::*;

/// The actual requests that we honor from an external source entity
#[derive(FromPrimitive, Copy, Clone, Debug, Eq, PartialEq)]
enum Request {
    None = 0,
    Start = 1,
    Hold = 2,
    Release = 3,
    Fault = 4,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Error {
    IllegalTask,
    BadTask,
    BadRequest,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct TaskIndex(u16);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Trace {
    None,
    Request(Request, TaskIndex),
    Disposition(TaskIndex, Disposition),
    Error(Error),
}

ringbuf!(Trace, 4, Trace::None);

#[no_mangle]
static JEFE_EXTERNAL_READY: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static JEFE_EXTERNAL_REQUEST: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static JEFE_EXTERNAL_TASKINDEX: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static JEFE_EXTERNAL_KICK: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static JEFE_EXTERNAL_REQUESTS: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
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
    false
}

///
/// Indicates that we are ready for external control.
///
pub fn set_ready() {
    JEFE_EXTERNAL_READY.fetch_add(1, Ordering::SeqCst);
}
