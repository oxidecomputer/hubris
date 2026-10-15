// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Host Task
//!
//! This module contains the state + interactions between the kernel process
//! and all task processes.

use crate::arch::Arch;
use crate::arch::host::{Host, config::config, fatal, task_name_from_idx};
use crate::task::{
    ArchState, BorrowArgs, PanicArgs, RecvArgs, ReplyArgs, SendArgs,
};
use crate::umem::USlice;
use abi::{
    Addr, FaultInfo, FaultSource, LeaseAttributes, Sysnum, TaskId, ULease,
    UsageError,
};
use hostcall::{IoError, Return, Syscall};
use std::cell::UnsafeCell;
use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The kernel's handle on a given task process.
pub(crate) struct TaskProcess {
    child: Child,
    to_task: ChildStdin,
    from_task: BufReader<ChildStdout>,
}

impl TaskProcess {
    /// Schedule the given task.
    ///
    /// Sends the `finished` response (if there is one), and waits for the next
    /// received system call (or some kind of fatal error).
    pub(crate) fn resume(
        &mut self,
        index: usize,
        finished: Option<Call>,
    ) -> Result<Call, FaultInfo> {
        let write_res = match finished {
            Some(call) => {
                // If we have a finished call to respond with, we'll need to
                // figure out which leases need to be written back. Currently,
                // we'll send back the full contents of any lease that had
                // writeable attributes, rather than trying to be more clever
                // than that.
                //
                // For leases that were only readable, we skip sending them
                // back, because modifications would have been illegal anyway!
                //
                // The lease_bufs are all in unsafecells so the kernel was able
                // to spookily poke them, but that's done now, so turn them back
                // into normal `Vec<u8>`s.
                let mut lease_writebacks = vec![];
                for (ul, buf) in
                    call.lease_table.iter().zip(call.lease_bufs.into_iter())
                {
                    if ul.attributes.contains(LeaseAttributes::WRITE) {
                        lease_writebacks.push(Some(buf.into_inner()));
                    } else {
                        lease_writebacks.push(None);
                    }
                }

                // We then give back the return values and output buf we've been
                // scribbling into, as well as the list of lease writebacks we
                // just prepared.
                let ret = Return {
                    rets: call.rets,
                    output: call.output_buf.into_inner(),
                    lease_writebacks,
                };

                // Do the sending, and return whether it was successful.
                hostcall::write_message(&mut self.to_task, &ret)
            }
            // Sent nothing successful!
            //
            // TODO(AJM): Should we enforce that we're only allowed to do this
            // once per task, when spawning?
            None => Ok(()),
        };

        // We want to handle "write errors" and "read errors" similarly, since
        // they both return the same kind of error. We also don't want to
        // attempt reading if the writing went poorly. This only performs the
        // latter if the former succeeded, and returns the common result type
        // between the two.
        let total_io_res = write_res
            .and_then(|()| hostcall::read_message(&mut self.from_task));

        // Now handle any errors received during I/O.
        match total_io_res {
            // The task was cool, and gave us a syscall! Turn it into the state
            // we need for the kernel, and return.
            Ok(Some(syscall)) => Ok(Call::new(syscall)),
            // The I/O failed in a way that says "oh no the task is dead",
            // by "hanging up" the child stdin connection we have. Try to figure
            // out exactly what happened, and return that back upwards.
            Ok(None) | Err(IoError::Eof | IoError::Io(_)) => {
                Err(exit_fault(&mut self.child))
            }
            // The I/O failed in a way that says "the task did something bad or
            // that we don't like", and since we don't like that, we're going to
            // go ahead and ruin the task's day by killing it. We then report
            // that up as a generic UsageError.
            Err(e @ IoError::Decode) => {
                eprintln!(
                    "kernel: bad request from task {}: {e}",
                    task_name_from_idx(index)
                );
                self.child.kill().ok();
                self.child.wait().ok();
                Err(FaultInfo::SyscallUsage(UsageError::BadSyscallNumber))
            }
        }
    }
}

/// Our table of all existing processes.
///
/// This needs to be a goofy singleton because the kernel expects to have
/// free-function style interfaces.
///
/// TODO(AJM): Maybe this could be a `BTreeMap<usize, TaskProcess>` instead of
/// a sparse array? it's probably fine either way.
static PROCESSES: OnceLock<Mutex<Vec<Option<TaskProcess>>>> = OnceLock::new();

/// Get all of our currently known process connections
pub(crate) fn processes() -> MutexGuard<'static, Vec<Option<TaskProcess>>> {
    PROCESSES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Starts (or restarts) the process for task `index`.
pub(crate) fn spawn_task(index: usize) {
    let cfg = config();
    let Some(desc) = cfg.tasks.get(index) else {
        fatal(format_args!(
            "task {index} is not described in the host configuration"
        ))
    };
    let mut procs = processes();
    if procs.len() <= index {
        procs.resize_with(index + 1, || None);
    }
    if let Some(mut old) = procs[index].take() {
        old.child.kill().ok();
        old.child.wait().ok();
    }

    // Idle tasks don't actually spawn processes, just return
    if desc.idle {
        return;
    }
    let Some(exe) = &desc.executable else {
        fatal(format_args!("task {} has no executable", desc.name))
    };
    let mut cmd = Command::new(exe);
    let slots = desc
        .slots
        .iter()
        .map(|(name, &index)| (name.as_str(), index));

    // Child tasks can use our stderr, we talk to them via piped stdio.
    cmd.env(hostcall::TASK_SLOTS_VAR, hostcall::task_slots_value(slots))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = cmd.spawn().unwrap_or_else(|e| {
        fatal(format_args!("launching {exe} for task {}: {e}", desc.name))
    });
    let stdout = child.stdout.take().expect("stdout was piped");
    let stdin = child.stdin.take().expect("stdin was piped");
    trace!("task {} started: {exe} (pid {})", desc.name, child.id());

    procs[index] = Some(TaskProcess {
        child,
        to_task: stdin,
        from_task: BufReader::new(stdout),
    });
}

/// Works out how a task process ended, after its pipe closed.
pub(crate) fn exit_fault(child: &mut Child) -> FaultInfo {
    let Ok(status) = child.wait() else {
        return FaultInfo::InvalidOperation(0);
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status.signal().is_some() {
            // A crash, most likely SIGSEGV: the nearest thing to an MPU fault.
            return FaultInfo::MemoryAccess {
                address: None,
                source: FaultSource::User,
            };
        }
    }
    match status.code() {
        Some(hostcall::EXIT_PANIC) => FaultInfo::Panic,
        Some(code) => FaultInfo::InvalidOperation(code as u32),
        None => FaultInfo::InvalidOperation(0),
    }
}

/// A `Call` is the context necessary in the kernel for representing a pending
/// [`Syscall`].
///
/// The lifecycle of a [`Call`] is:
///
/// 1. The task sends us a [`Syscall`], containing the serialized form of the
///    syscall ID, register args, input buffer, and any leases.
/// 2. We convert this to a [`Call`] using [`Call::new()`], which consumes the
///    information provided by the task, and creates the "scratch" data that
///    the kernel is allowed to write to (register rets, output buffer, and
///    any writable leases).
/// 3. The kernel directly writes into the scratch buffers, or allows other
///    tasks to indirectly write to the scratch buffers, for example with REPLY.
/// 4. At some future point, the kernel decides to resume a task, at which point
///    the [`Call`] is destructed, and the scratch parts are used to form a new
///    [`Return`] value. This is sent back to the task.
///
/// Because the kernel expects to directly interact with a Task's memory range,
/// which we can't do here (easily) because the Task is an entirely different
/// process, [`Call`] acts as a proxy. This means the guts of [`Call`] are a
/// little awkward, so that we can give the kernel reasonable pointers to
/// (soundly) readable/writable memory!
#[derive(Debug)]
pub(crate) struct Call {
    /// The [`Syscall::nr`] requested
    pub(crate) syscall_nr: Sysnum,
    /// The [`Syscall::args`] sent to the kernel
    pub(crate) args: [u32; 7],
    /// The [`Return::rets`] we will send back to the task
    pub(crate) rets: [u32; 6],
    // These are the actual storage locations of the items that will be accessed
    // by the kernel. Don't touch these until we're turning these back into the
    // [`Return`] when consuming [`Call`]!
    lease_bufs: Vec<UnsafeCell<Vec<u8>>>,
    _input_buf: UnsafeCell<Vec<u8>>,
    output_buf: UnsafeCell<Vec<u8>>,
    /// The lease table of a SEND, as the array of `ULease` the kernel
    /// expects to find in task memory.
    pub(crate) lease_table: Vec<ULease>,
    /// The `input` slice, pointing to the payload of [`Self::_input_buf`].
    input: USlice<u8>,
    /// The `output` slice, pointing to the payload of [`Self::output_buf`].
    output: USlice<u8>,
}

impl Call {
    pub(crate) fn new(syscall: Syscall) -> Self {
        let mut attr_table = vec![];
        let mut lease_bufs = vec![];

        // Build the lease table. For each lease, we need to either take the
        // given buffer (for readable leases), or make a new empty buffer (for
        // write only leases).
        //
        // We put the buffers into an unsafecell, because they will be accessed
        // indirectly/spookily using the lease table we will build below. Once
        // we've put the buf in the unsafe cell, we should be careful to never
        // resize it or do anything else that would invalidate the ptr or its
        // provenance.
        for lease in syscall.leases {
            let read = lease.attributes.contains(LeaseAttributes::READ);
            let write = lease.attributes.contains(LeaseAttributes::WRITE);
            match (read, write) {
                (false, false) => {
                    // What are we doing here? We may want to turn this into
                    // something that kills the task instead for doing something
                    // silly, instead of killing the kernel, but this is okay
                    // for now, I think.
                    todo!("Got a lease with no read or write privs");
                }
                (false, true) => {
                    // Write only, new buffer with given len
                    //
                    // TODO: For write-only buffers, this also zeroes the entire
                    // buffer (if the entire buffer wasn't written), which is
                    // allowable but perhaps not accurate. If we need to track
                    // "dirty" status on a per-byte basis, we can also do that,
                    // but we don't yet.
                    let len = lease.len as usize;
                    lease_bufs.push(UnsafeCell::new(vec![0; len]));
                    attr_table.push((lease.attributes, len));
                }
                (true, _) => {
                    // Readable/Read+Write, use incoming buffer
                    let len = lease.contents.len();
                    assert_eq!(lease.len as usize, lease.contents.len());
                    lease_bufs.push(UnsafeCell::new(lease.contents));
                    attr_table.push((lease.attributes, len));
                }
            }
        }

        // We build the lease table AFTER all the buffers are in a vec, so that
        // we know that they won't move and invalidate the ptr of the ULease we
        // are about to build.
        //
        // This is an attempt to do provenance right, it might be a little over
        // cautious, and I am open to other ideas.
        let mut lease_table = vec![];
        for ((attr, len), buf) in attr_table.iter().zip(lease_bufs.iter()) {
            let base_ptr: *mut u8 = unsafe {
                let v: &mut Vec<u8> = &mut *buf.get();
                v.as_mut_ptr()
            };
            lease_table.push(ULease {
                attributes: *attr,
                base_address: Addr::from_ptr(base_ptr),
                length: *len,
            });
        }

        // We also put the input/output into an unsafecell, they will be
        // accessed spookily by the kernel.
        let _input_buf = UnsafeCell::new(syscall.input);
        let output_buf = UnsafeCell::new(vec![0; syscall.output_len as usize]);

        // Now that the buffers are in an unsafecell, create the uslice from
        // pointers derived from these items. The pointee buffer does not move,
        // even though we move the `UnsafeCell<Vec<u8>>`s into Self.
        let output = {
            let o: &mut Vec<u8> = unsafe { &mut *output_buf.get() };
            let len = o.len();
            let optr: *mut u8 = o.as_mut_ptr();
            USlice::from_raw(optr as usize, len)
                .expect("Making a USlice from a vec should never fail")
        };
        let input = {
            let i: &mut Vec<u8> = unsafe { &mut *_input_buf.get() };
            let len = i.len();
            let iptr: *const u8 = i.as_ptr();
            USlice::from_raw(iptr as usize, len)
                .expect("Making a USlice from a vec should never fail")
        };

        Self {
            args: syscall.args,
            syscall_nr: syscall.nr,
            rets: [0; 6],
            _input_buf,
            output_buf,
            lease_bufs,
            lease_table,
            input,
            output,
        }
    }

    /// The buffer the syscall passes in, as a slice of task memory.
    pub(crate) fn input(&self) -> Result<USlice<u8>, UsageError> {
        Ok(self.input.clone())
    }

    /// The buffer the syscall gets back, as a slice of task memory.
    pub(crate) fn output(&self) -> Result<USlice<u8>, UsageError> {
        Ok(self.output.clone())
    }
}

/// Per-task architecture state
#[derive(Debug, Default)]
pub struct SavedState {
    call: Option<Call>,
}

impl SavedState {
    /// Overwrite the existing [`Call`].
    pub(crate) fn set_call(&mut self, call: Call) {
        self.call = Some(call);
    }

    /// Take the existing [`Call`], if any.
    pub(crate) fn take_call(&mut self) -> Option<Call> {
        self.call.take()
    }

    /// Read access to the [`Call`].
    ///
    /// Panics if there is no pending [`Call`].
    fn call(&self) -> &Call {
        match &self.call {
            Some(call) => call,
            None => fatal(
                "the kernel asked for the syscall arguments of a task that \
                 is not in a syscall",
            ),
        }
    }

    /// Write access to the [`Call`].
    ///
    /// Panics if there is no pending [`Call`].
    fn call_mut(&mut self) -> &mut Call {
        match &mut self.call {
            Some(call) => call,
            None => fatal(
                "the kernel asked for the syscall arguments of a task that \
                 is not in a syscall",
            ),
        }
    }
}

impl ArchState for SavedState {
    fn stack_pointer(&self) -> u32 {
        0
    }

    //
    // We can read args from the values that we obtained through the [`Syscall`]
    // message from the task. Same deal for the syscall number.
    //

    fn arg0(&self) -> u32 {
        self.call().args[0]
    }
    fn arg1(&self) -> u32 {
        self.call().args[1]
    }
    fn arg2(&self) -> u32 {
        self.call().args[2]
    }
    fn arg3(&self) -> u32 {
        self.call().args[3]
    }
    fn arg4(&self) -> u32 {
        self.call().args[4]
    }
    fn arg5(&self) -> u32 {
        self.call().args[5]
    }
    fn arg6(&self) -> u32 {
        self.call().args[6]
    }

    fn syscall_descriptor(&self) -> u32 {
        self.call().syscall_nr as u32
    }

    //
    // Writing the return value writes to the scratch buffers that we will later
    // send back in the [`Return`] message to the task.
    //

    fn ret0(&mut self, value: u32) {
        self.call_mut().rets[0] = value;
    }
    fn ret1(&mut self, value: u32) {
        self.call_mut().rets[1] = value;
    }
    fn ret2(&mut self, value: u32) {
        self.call_mut().rets[2] = value;
    }
    fn ret3(&mut self, value: u32) {
        self.call_mut().rets[3] = value;
    }
    fn ret4(&mut self, value: u32) {
        self.call_mut().rets[4] = value;
    }
    fn ret5(&mut self, value: u32) {
        self.call_mut().rets[5] = value;
    }

    //
    // This is the part where we do the sneaky swap: when the kernel asks for
    // the buffers relevant for these syscalls, we give it the memory locations
    // and data stored in [`Call`] instead of in the task's address space. The
    // kernel is none the wiser, and we'll "flush" this back to the task when
    // we return control flow back to it.
    //

    fn as_send_args(&self) -> SendArgs {
        let call = self.call();
        SendArgs {
            callee: TaskId((self.arg0() >> 16) as u16),
            operation: self.arg0() as u16,
            message: call.input(),
            response: call.output(),
            lease_table: USlice::from_raw(
                call.lease_table.as_ptr() as usize,
                call.lease_table.len(),
            ),
        }
    }

    fn as_recv_args(&self) -> RecvArgs {
        let sender = self.arg3();
        RecvArgs {
            buffer: self.call().output(),
            notification_mask: self.arg2(),
            specific_sender: (sender & (1 << 31) != 0)
                .then_some(TaskId(sender as u16)),
        }
    }

    fn as_reply_args(&self) -> ReplyArgs {
        ReplyArgs {
            callee: TaskId(self.arg0() as u16),
            response_code: self.arg1(),
            message: self.call().input(),
        }
    }

    fn as_borrow_args(&self) -> BorrowArgs {
        let call = self.call();
        BorrowArgs {
            lender: TaskId(self.arg0() as u16),
            lease_number: self.arg1() as usize,
            offset: self.arg2() as usize,
            buffer: if call.syscall_nr == Sysnum::BorrowWrite {
                call.input()
            } else {
                call.output()
            },
        }
    }

    fn as_panic_args(&self) -> PanicArgs {
        PanicArgs {
            message: self.call().input(),
        }
    }
}
