// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Host architecture: the kernel as an ordinary process, tasks as children.
//!
//! This is what the kernel compiles to when it is built for anything other
//! than a Hubris target. Nothing about the portable kernel changes; what
//! changes is what "running a task" and "a task made a syscall" mean.
//!
//! # Model
//!
//! Every task is a separate process, built for the host with userlib's host
//! syscall implementation, which turns each syscall into a request on the
//! process's stdout followed by a blocking read of the response on its stdin
//! (the `hostcall` crate). The kernel launches those processes and holds the
//! pipes.
//!
//! The kernel is single-threaded and exactly one task is *current*, as on the
//! real hardware. Resuming the current task means sending it the response to
//! the syscall it is blocked in; the kernel then waits for that task's next
//! request, which is the moment the SVC handler would run on hardware.
//! Non-current tasks are all blocked in a syscall of their own, whether the
//! kernel has read it yet or not: a freshly started process runs until its
//! first syscall and then sits on the pipe until the scheduler gets to it.
//!
//! Syscall arguments that are pointers into task memory on hardware are
//! byte vectors here. Each request's buffers are owned by the kernel, and the
//! [`SavedState`] hands the portable code `USlice`s that point into them, so
//! the region checks, `safe_copy`, and lease borrowing all run unchanged on
//! kernel-side memory. Results the portable code writes back through
//! `ArchState` are captured and sent to the task when it is next resumed.
//!
//! Time is virtual. When the scheduler picks the idle task, the clock jumps
//! to the earliest pending timer deadline and the timers are processed. A
//! run therefore has no real-time dependence at all, and ends when every task
//! is blocked with no timer pending (or when the configured stop time is
//! reached).
//!
//! Memory protection does not apply (the kernel never touches task memory),
//! interrupts exist only as software-pended notifications, and a task process
//! that dies is faulted the way a crashed task would be.
//!
//! # Configuration
//!
//! `HUBRIS_HOST_CONFIG` names a RON file with a [`HostConfig`], produced by
//! `cargo xtask host-run`: one entry per task in task-index order, giving the
//! executable to launch and the task's `task_slot!` name-to-index map, which
//! on hardware is patched into the binary after linking, and optionally the
//! `.idol` file of the interface the task serves, so that traffic to it can
//! be decoded.
//!
//! Set `HUBRIS_HOST_TRACE` to log syscalls to stderr: `1` (or `all`) logs
//! everything, and a comma-separated list of task names logs only the
//! syscalls made by, or addressed to, those tasks. Sends and replies to a
//! task with a known interface are shown as decoded operations, such as
//! `SEND to sensor: post(id: SensorId(3), value: 31.5, timestamp: 1000)`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Display;
use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use abi::{
    Addr, FaultInfo, FaultSource, InterruptNum, IrqStatus, LeaseAttributes,
    ReplyFaultReason, Sysnum, TaskId, ULease, UsageError,
};
use hostcall::nprpc::interface::Method;
use hostcall::nprpc::io::server::{Io as _, RawIoFrame};
use hostcall::nprpc::wire::{Header, Operation};
use hostcall::runtime::methods as runtime_methods;
use hostcall::syscalls::methods as syscall_methods;
use hostcall::{
    BorrowInfo, BorrowInfoRequest, BorrowReadRequest, BorrowReadResponse,
    BorrowWriteRequest, BorrowWriteResponse, Fault, IrqControlRequest,
    PanicRequest, PostRequest, RecvMessage, RecvRequest, RecvResponse,
    ReplyFaultRequest, ReplyRequest, SendRequest, SendResponse,
    SetTimerRequest, StreamIo, TimerState,
};
use idol_trace::Decoder;
use serde::{Deserialize, Serialize};

use crate::atomic::AtomicExt;
use crate::descs::RegionAttributes;
use crate::startup::with_task_table;
use crate::task::{
    self, ArchState, BorrowArgs, IrqArgs, IrqStatusArgs, NotificationSet,
    PanicArgs, PostArgs, RecvArgs, RefreshTaskIdArgs, ReplyArgs,
    ReplyFaultArgs, SendArgs, SetTimerArgs, Task,
};
use crate::time::Timestamp;
use crate::umem::USlice;

/////////////////////////////////////////////////////////////////////////////
// Configuration

/// Description of a host run, loaded from the file named by
/// `HUBRIS_HOST_CONFIG`.
#[derive(Debug, Deserialize, Serialize)]
pub struct HostConfig {
    /// One entry per task, in task-index order (the order of the app
    /// manifest).
    pub tasks: Vec<HostTask>,
    /// Stop the run once virtual time would advance past this tick.
    #[serde(default)]
    pub stop_at: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HostTask {
    pub name: String,
    /// Program to run for this task. Not needed for the idle task.
    #[serde(default)]
    pub executable: Option<String>,
    /// `task_slot!` name to task index, from the manifest's `task-slots`.
    #[serde(default)]
    pub slots: BTreeMap<String, u16>,
    /// The idle task never runs: selecting it advances virtual time instead.
    #[serde(default)]
    pub idle: bool,
    /// The `.idol` file of the interface this task serves, if any, for
    /// decoding traffic to it in the trace.
    #[serde(default)]
    pub interface: Option<String>,
}

static CONFIG: OnceLock<HostConfig> = OnceLock::new();

fn config() -> &'static HostConfig {
    CONFIG.get_or_init(|| {
        let path = std::env::var("HUBRIS_HOST_CONFIG").unwrap_or_else(|_| {
            fatal(
                "HUBRIS_HOST_CONFIG is not set; it must name the RON file \
                 describing the task processes (see `cargo xtask host-run`)",
            )
        });
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| fatal(format_args!("reading {path}: {e}")));
        ron::from_str(&text)
            .unwrap_or_else(|e| fatal(format_args!("parsing {path}: {e}")))
    })
}

/// What `HUBRIS_HOST_TRACE` asked for.
enum TraceFilter {
    Off,
    All,
    /// Only syscalls involving one of these tasks.
    Tasks(HashSet<String>),
}

fn trace_filter() -> &'static TraceFilter {
    static FILTER: OnceLock<TraceFilter> = OnceLock::new();
    FILTER.get_or_init(|| match std::env::var("HUBRIS_HOST_TRACE") {
        Err(_) => TraceFilter::Off,
        Ok(v) if v.is_empty() || v == "1" || v == "all" => TraceFilter::All,
        Ok(v) => TraceFilter::Tasks(
            v.split(',').map(|s| s.trim().to_string()).collect(),
        ),
    })
}

fn tracing() -> bool {
    !matches!(trace_filter(), TraceFilter::Off)
}

/// Whether a syscall involving these tasks (the caller, and any peer) should
/// be traced.
fn traced(participants: &[usize]) -> bool {
    match trace_filter() {
        TraceFilter::Off => false,
        TraceFilter::All => true,
        TraceFilter::Tasks(names) => {
            participants.iter().any(|&i| names.contains(task_name(i)))
        }
    }
}

macro_rules! trace {
    ($($arg:tt)*) => {
        if tracing() {
            eprintln!("[kernel t={}] {}", TICKS.load(Ordering::Relaxed),
                format_args!($($arg)*));
        }
    };
}

/// Like `trace!`, for a syscall by `index` involving `participants`.
macro_rules! trace_task {
    ($participants:expr, $($arg:tt)*) => {
        if traced($participants) {
            eprintln!("[kernel t={}] {}", TICKS.load(Ordering::Relaxed),
                format_args!($($arg)*));
        }
    };
}

fn task_name(index: usize) -> &'static str {
    match config().tasks.get(index) {
        Some(task) => &task.name,
        None if index == TaskId::KERNEL.index() => "kernel",
        None => "?",
    }
}

/// Interface decoders, one per task, for tasks whose configuration names an
/// `.idol` file.
fn decoders() -> &'static [Option<Decoder>] {
    static DECODERS: OnceLock<Vec<Option<Decoder>>> = OnceLock::new();
    DECODERS.get_or_init(|| {
        config()
            .tasks
            .iter()
            .map(|task| {
                let path = task.interface.as_ref()?;
                match Decoder::load(std::path::Path::new(path)) {
                    Ok(decoder) => Some(decoder),
                    Err(e) => {
                        eprintln!(
                            "kernel: not decoding traffic to {}: {e:#}",
                            task.name
                        );
                        None
                    }
                }
            })
            .collect()
    })
}

/// Sends in flight, by sender: the target and operation, so that the reply
/// can be decoded against the operation it answers.
static PENDING_SENDS: Mutex<BTreeMap<usize, (usize, u16)>> =
    Mutex::new(BTreeMap::new());

/// Describes a syscall for the trace, decoding IPC where the peer's
/// interface is known. Returns the text and the peer task, if any.
fn describe_syscall(index: usize, kind: &CallKind) -> (String, Option<usize>) {
    let mut pending = PENDING_SENDS.lock().unwrap_or_else(|e| e.into_inner());
    match kind {
        CallKind::Send {
            target,
            operation,
            message,
            leases,
            ..
        } => {
            let peer = target.index();
            pending.insert(index, (peer, *operation));
            let what = match decoders().get(peer).and_then(Option::as_ref) {
                Some(decoder) => {
                    decoder.describe_request(*operation, &message.data)
                }
                None => {
                    format!("op {operation} ({} bytes)", message.data.len())
                }
            };
            let leases = match leases.len() {
                0 => String::new(),
                n => format!(" [{n} leases]"),
            };
            (
                format!("SEND to {}: {what}{leases}", task_name(peer)),
                Some(peer),
            )
        }
        CallKind::Reply {
            peer,
            code,
            message,
        } => {
            let peer = peer.index();
            let what = match (
                pending.remove(&peer),
                decoders().get(index).and_then(Option::as_ref),
            ) {
                (Some((target, op)), Some(decoder)) if target == index => {
                    format!(
                        "{} -> {}",
                        decoder.op_name(op).unwrap_or("?"),
                        decoder.describe_reply(op, *code, &message.data)
                    )
                }
                _ => format!("code {code} ({} bytes)", message.data.len()),
            };
            (format!("REPLY to {}: {what}", task_name(peer)), Some(peer))
        }
        CallKind::ReplyFault { peer, reason } => {
            let peer = peer.index();
            let op = pending
                .remove(&peer)
                .and_then(|(target, op)| (target == index).then_some(op))
                .and_then(|op| {
                    decoders()
                        .get(index)
                        .and_then(Option::as_ref)
                        .and_then(|d| d.op_name(op).map(str::to_string))
                })
                .unwrap_or_default();
            (
                format!(
                    "REPLY_FAULT to {}: {op} reason {reason}",
                    task_name(peer)
                ),
                Some(peer),
            )
        }
        other => (other.describe(), None),
    }
}

/// Ends the run because the kernel itself cannot continue; this is the host
/// analogue of a kernel panic, not of a task fault.
fn fatal(msg: impl Display) -> ! {
    eprintln!("kernel: {msg}");
    std::process::exit(2)
}

/// Ends the run normally: the simulated system has nothing left to do.
fn finish(msg: impl Display) -> ! {
    eprintln!("kernel: stopping: {msg}");
    std::process::exit(0)
}

/////////////////////////////////////////////////////////////////////////////
// Task processes

/// Room for one request frame; a task's message plus its leases must fit.
const FRAME_BUFFER_LEN: usize = 1 << 20;

struct TaskProcess {
    child: Child,
    io: StreamIo<BufReader<ChildStdout>, ChildStdin>,
    buf: Vec<u8>,
}

static PROCESSES: OnceLock<Mutex<Vec<Option<TaskProcess>>>> = OnceLock::new();

fn processes() -> MutexGuard<'static, Vec<Option<TaskProcess>>> {
    PROCESSES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Starts (or restarts) the process for task `index`.
fn spawn_task(index: usize) {
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
    if desc.idle {
        return;
    }
    let Some(exe) = &desc.executable else {
        fatal(format_args!("task {} has no executable", desc.name))
    };
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::piped())
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
        io: StreamIo::new(BufReader::new(stdout), stdin),
        buf: vec![0; FRAME_BUFFER_LEN],
    });
}

/// Works out how a task process ended, after its pipe closed.
fn exit_fault(child: &mut Child) -> FaultInfo {
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

/////////////////////////////////////////////////////////////////////////////
// Kernel-side task memory

/// A byte buffer standing in for a region of task memory.
///
/// The address is recorded from the mutable pointer when the buffer is
/// created, so that `USlice`s built from it may be written through; the heap
/// allocation does not move when the `Buffer` does.
#[derive(Debug)]
struct Buffer {
    data: Vec<u8>,
    address: usize,
}

impl Buffer {
    fn new(mut data: Vec<u8>) -> Self {
        let address = data.as_mut_ptr() as usize;
        Self { data, address }
    }

    fn zeroed(len: usize) -> Self {
        Self::new(vec![0; len])
    }

    fn uslice(&self) -> Result<USlice<u8>, UsageError> {
        USlice::from_raw(self.address, self.data.len())
    }

    /// The first `len` bytes, or all of them if `len` is larger.
    fn prefix(&self, len: usize) -> Vec<u8> {
        self.data[..len.min(self.data.len())].to_vec()
    }
}

/// A lease's bytes on the kernel side, plus the attributes the task claimed.
#[derive(Debug)]
struct LeaseBuffer {
    attributes: LeaseAttributes,
    buffer: Buffer,
}

/// The lease table itself, as the array of `ULease` the kernel expects to
/// find in task memory.
#[derive(Debug)]
struct LeaseTable {
    entries: Vec<ULease>,
    address: usize,
}

impl LeaseTable {
    fn new(mut entries: Vec<ULease>) -> Self {
        let address = entries.as_mut_ptr() as usize;
        Self { entries, address }
    }

    fn uslice(&self) -> Result<USlice<ULease>, UsageError> {
        USlice::from_raw(self.address, self.entries.len())
    }
}

/////////////////////////////////////////////////////////////////////////////
// Saved state: the syscall a task is in

/// Per-task architecture state: the syscall the task is currently blocked
/// in, with its buffers and (once the kernel has produced it) its result.
#[derive(Debug, Default)]
pub struct SavedState {
    call: Option<Call>,
}

#[derive(Debug)]
struct Call {
    /// The request header, echoed in the response so the task's client can
    /// match them up.
    header: Header,
    kind: CallKind,
    result: Option<CallResult>,
}

#[derive(Debug)]
enum CallKind {
    Send {
        target: TaskId,
        operation: u16,
        message: Buffer,
        reply: Buffer,
        leases: Vec<LeaseBuffer>,
        lease_table: LeaseTable,
    },
    Recv {
        buffer: Buffer,
        notification_mask: u32,
        specific_sender: Option<TaskId>,
    },
    Reply {
        peer: TaskId,
        code: u32,
        message: Buffer,
    },
    SetTimer {
        deadline: Option<u64>,
        notifications: u32,
    },
    BorrowRead {
        lender: TaskId,
        index: usize,
        offset: usize,
        buffer: Buffer,
    },
    BorrowWrite {
        lender: TaskId,
        index: usize,
        offset: usize,
        data: Buffer,
    },
    BorrowInfo {
        lender: TaskId,
        index: usize,
    },
    IrqControl {
        mask: u32,
        flags: u32,
    },
    Panic {
        message: Buffer,
    },
    GetTimer,
    RefreshTaskId {
        task_id: TaskId,
    },
    Post {
        task: TaskId,
        bits: u32,
    },
    ReplyFault {
        peer: TaskId,
        reason: u32,
    },
    IrqStatus {
        mask: u32,
    },
}

impl CallKind {
    fn sysnum(&self) -> Sysnum {
        match self {
            Self::Send { .. } => Sysnum::Send,
            Self::Recv { .. } => Sysnum::Recv,
            Self::Reply { .. } => Sysnum::Reply,
            Self::SetTimer { .. } => Sysnum::SetTimer,
            Self::BorrowRead { .. } => Sysnum::BorrowRead,
            Self::BorrowWrite { .. } => Sysnum::BorrowWrite,
            Self::BorrowInfo { .. } => Sysnum::BorrowInfo,
            Self::IrqControl { .. } => Sysnum::IrqControl,
            Self::Panic { .. } => Sysnum::Panic,
            Self::GetTimer => Sysnum::GetTimer,
            Self::RefreshTaskId { .. } => Sysnum::RefreshTaskId,
            Self::Post { .. } => Sysnum::Post,
            Self::ReplyFault { .. } => Sysnum::ReplyFault,
            Self::IrqStatus { .. } => Sysnum::IrqStatus,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Send {
                target,
                operation,
                message,
                leases,
                ..
            } => format!(
                "SEND to {} op {operation} ({} bytes, {} leases)",
                target.index(),
                message.data.len(),
                leases.len()
            ),
            Self::Recv {
                notification_mask,
                specific_sender,
                ..
            } => format!(
                "RECV mask {notification_mask:#x} from {:?}",
                specific_sender.map(|t| t.index())
            ),
            Self::Reply { peer, code, .. } => {
                format!("REPLY to {} code {code}", peer.index())
            }
            Self::SetTimer {
                deadline,
                notifications,
            } => format!("SET_TIMER {deadline:?} bits {notifications:#x}"),
            Self::Panic { message } => {
                format!("PANIC: {}", String::from_utf8_lossy(&message.data))
            }
            other => sysnum_name(other.sysnum()).to_string(),
        }
    }
}

fn sysnum_name(n: Sysnum) -> &'static str {
    match n {
        Sysnum::Send => "SEND",
        Sysnum::Recv => "RECV",
        Sysnum::Reply => "REPLY",
        Sysnum::SetTimer => "SET_TIMER",
        Sysnum::BorrowRead => "BORROW_READ",
        Sysnum::BorrowWrite => "BORROW_WRITE",
        Sysnum::BorrowInfo => "BORROW_INFO",
        Sysnum::IrqControl => "IRQ_CONTROL",
        Sysnum::Panic => "PANIC",
        Sysnum::GetTimer => "GET_TIMER",
        Sysnum::RefreshTaskId => "REFRESH_TASK_ID",
        Sysnum::Post => "POST",
        Sysnum::ReplyFault => "REPLY_FAULT",
        Sysnum::IrqStatus => "IRQ_STATUS",
    }
}

/// What the portable kernel handed back through `ArchState`.
#[derive(Debug, Clone, Copy)]
enum CallResult {
    Error(u32),
    Send {
        code: u32,
        len: usize,
    },
    Recv {
        sender: TaskId,
        operation: u32,
        len: usize,
        response_capacity: usize,
        lease_count: usize,
    },
    Borrow {
        code: u32,
        len: usize,
    },
    BorrowInfo {
        attributes: u32,
        len: usize,
    },
    Time {
        now: u64,
        deadline: Option<u64>,
        on_deadline: u32,
    },
    TaskId(TaskId),
    IrqStatus(u32),
}

impl SavedState {
    fn kind(&self) -> &CallKind {
        match &self.call {
            Some(call) => &call.kind,
            None => fatal(
                "the kernel asked for the syscall arguments of a task that \
                 is not in a syscall",
            ),
        }
    }

    fn set_result(&mut self, result: CallResult) {
        match &mut self.call {
            Some(call) => call.result = Some(result),
            None => trace!("discarding a syscall result for an idle task"),
        }
    }
}

fn wrong_syscall(expected: &str, actual: &CallKind) -> ! {
    fatal(format_args!(
        "the kernel decoded {expected} arguments while the task is in {}",
        sysnum_name(actual.sysnum())
    ))
}

/// An empty, well-aligned byte slice, for argument slots a syscall doesn't
/// use.
fn no_bytes() -> Result<USlice<u8>, UsageError> {
    USlice::from_raw(core::mem::align_of::<u8>(), 0)
}

impl ArchState for SavedState {
    // The register-shaped accessors are never consulted: every `as_*_args`
    // and `set_*` method is overridden below to work on the decoded request.
    fn stack_pointer(&self) -> u32 {
        0
    }
    fn arg0(&self) -> u32 {
        0
    }
    fn arg1(&self) -> u32 {
        0
    }
    fn arg2(&self) -> u32 {
        0
    }
    fn arg3(&self) -> u32 {
        0
    }
    fn arg4(&self) -> u32 {
        0
    }
    fn arg5(&self) -> u32 {
        0
    }
    fn arg6(&self) -> u32 {
        0
    }

    fn syscall_descriptor(&self) -> u32 {
        match &self.call {
            Some(call) => call.kind.sysnum() as u32,
            None => u32::MAX,
        }
    }

    fn ret0(&mut self, _: u32) {}
    fn ret1(&mut self, _: u32) {}
    fn ret2(&mut self, _: u32) {}
    fn ret3(&mut self, _: u32) {}
    fn ret4(&mut self, _: u32) {}
    fn ret5(&mut self, _: u32) {}

    fn as_send_args(&self) -> SendArgs {
        match self.kind() {
            CallKind::Send {
                target,
                operation,
                message,
                reply,
                lease_table,
                ..
            } => SendArgs {
                callee: *target,
                operation: *operation,
                message: message.uslice(),
                response: reply.uslice(),
                lease_table: lease_table.uslice(),
            },
            other => wrong_syscall("SEND", other),
        }
    }

    fn as_recv_args(&self) -> RecvArgs {
        match self.kind() {
            CallKind::Recv {
                buffer,
                notification_mask,
                specific_sender,
            } => RecvArgs {
                buffer: buffer.uslice(),
                notification_mask: *notification_mask,
                specific_sender: *specific_sender,
            },
            other => wrong_syscall("RECV", other),
        }
    }

    fn as_reply_args(&self) -> ReplyArgs {
        match self.kind() {
            CallKind::Reply {
                peer,
                code,
                message,
            } => ReplyArgs {
                callee: *peer,
                response_code: *code,
                message: message.uslice(),
            },
            other => wrong_syscall("REPLY", other),
        }
    }

    fn as_reply_fault_args(&self) -> ReplyFaultArgs {
        match self.kind() {
            CallKind::ReplyFault { peer, reason } => ReplyFaultArgs {
                callee: *peer,
                reason: ReplyFaultReason::try_from(*reason)
                    .map_err(|_| UsageError::BadReplyFaultReason),
            },
            other => wrong_syscall("REPLY_FAULT", other),
        }
    }

    fn as_set_timer_args(&self) -> SetTimerArgs {
        match self.kind() {
            CallKind::SetTimer {
                deadline,
                notifications,
            } => SetTimerArgs {
                deadline: deadline.map(Timestamp::from),
                notification: NotificationSet(*notifications),
            },
            other => wrong_syscall("SET_TIMER", other),
        }
    }

    fn as_borrow_args(&self) -> BorrowArgs {
        match self.kind() {
            CallKind::BorrowRead {
                lender,
                index,
                offset,
                buffer,
            } => BorrowArgs {
                lender: *lender,
                lease_number: *index,
                offset: *offset,
                buffer: buffer.uslice(),
            },
            CallKind::BorrowWrite {
                lender,
                index,
                offset,
                data,
            } => BorrowArgs {
                lender: *lender,
                lease_number: *index,
                offset: *offset,
                buffer: data.uslice(),
            },
            CallKind::BorrowInfo { lender, index } => BorrowArgs {
                lender: *lender,
                lease_number: *index,
                offset: 0,
                buffer: no_bytes(),
            },
            other => wrong_syscall("BORROW", other),
        }
    }

    fn as_irq_args(&self) -> IrqArgs {
        match self.kind() {
            CallKind::IrqControl { mask, flags } => IrqArgs {
                notification_bitmask: *mask,
                control: *flags,
            },
            other => wrong_syscall("IRQ_CONTROL", other),
        }
    }

    fn as_panic_args(&self) -> PanicArgs {
        match self.kind() {
            CallKind::Panic { message } => PanicArgs {
                message: message.uslice(),
            },
            other => wrong_syscall("PANIC", other),
        }
    }

    fn as_refresh_task_id_args(&self) -> RefreshTaskIdArgs {
        match self.kind() {
            CallKind::RefreshTaskId { task_id } => {
                RefreshTaskIdArgs { task_id: *task_id }
            }
            other => wrong_syscall("REFRESH_TASK_ID", other),
        }
    }

    fn as_post_args(&self) -> PostArgs {
        match self.kind() {
            CallKind::Post { task, bits } => PostArgs {
                task_id: *task,
                notification_bits: NotificationSet(*bits),
            },
            other => wrong_syscall("POST", other),
        }
    }

    fn as_irq_status_args(&self) -> IrqStatusArgs {
        match self.kind() {
            CallKind::IrqStatus { mask } => IrqStatusArgs {
                notification_bitmask: *mask,
            },
            other => wrong_syscall("IRQ_STATUS", other),
        }
    }

    fn set_error_response(&mut self, resp: u32) {
        self.set_result(CallResult::Error(resp));
    }

    fn set_send_response_and_length(&mut self, resp: u32, len: usize) {
        self.set_result(CallResult::Send { code: resp, len });
    }

    fn set_recv_result(
        &mut self,
        sender: TaskId,
        operation: u32,
        length: usize,
        response_capacity: usize,
        lease_count: usize,
    ) {
        self.set_result(CallResult::Recv {
            sender,
            operation,
            len: length,
            response_capacity,
            lease_count,
        });
    }

    fn set_borrow_response_and_length(&mut self, resp: u32, len: usize) {
        self.set_result(CallResult::Borrow { code: resp, len });
    }

    fn set_borrow_info(&mut self, atts: u32, len: usize) {
        self.set_result(CallResult::BorrowInfo {
            attributes: atts,
            len,
        });
    }

    fn set_time_result(
        &mut self,
        now: Timestamp,
        dl: Option<Timestamp>,
        not: NotificationSet,
    ) {
        self.set_result(CallResult::Time {
            now: now.into(),
            deadline: dl.map(u64::from),
            on_deadline: not.0,
        });
    }

    fn set_refresh_task_id_result(&mut self, id: TaskId) {
        self.set_result(CallResult::TaskId(id));
    }

    fn set_irq_status_result(&mut self, status: IrqStatus) {
        self.set_result(CallResult::IrqStatus(status.bits()));
    }
}

/////////////////////////////////////////////////////////////////////////////
// Requests and responses

enum Incoming {
    Syscall(Call),
    TaskSlot { header: Header, name: String },
    Gone(FaultInfo),
}

fn read_incoming(proc: &mut TaskProcess) -> Incoming {
    let decoded = match proc.io.recv_one_frame_raw(&mut proc.buf) {
        Ok(Some(frame)) => decode(frame.raw),
        Ok(None) => return Incoming::Gone(exit_fault(&mut proc.child)),
        Err(e) => {
            trace!("reading from the task failed: {e:?}");
            return Incoming::Gone(exit_fault(&mut proc.child));
        }
    };
    match decoded {
        Ok(incoming) => incoming,
        Err(problem) => {
            eprintln!("kernel: bad request from task: {problem}");
            proc.child.kill().ok();
            proc.child.wait().ok();
            Incoming::Gone(FaultInfo::SyscallUsage(
                UsageError::BadSyscallNumber,
            ))
        }
    }
}

fn decode(raw: &[u8]) -> Result<Incoming, String> {
    let (header, body) = postcard::take_from_bytes::<Header>(raw)
        .map_err(|e| format!("bad frame header: {e}"))?;
    if header.op != Operation::Request {
        return Err(format!("unexpected frame operation {:?}", header.op));
    }

    fn parse<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, String> {
        postcard::from_bytes(body).map_err(|e| format!("bad request body: {e}"))
    }

    let key = header.key;
    let kind = if key == <syscall_methods::send as Method>::KEY {
        let r: SendRequest = parse(body)?;
        let leases: Vec<LeaseBuffer> = r
            .leases
            .into_iter()
            .map(|lease| {
                let attributes =
                    LeaseAttributes::from_bits_truncate(lease.attributes);
                let mut data = if attributes.contains(LeaseAttributes::READ) {
                    lease.contents
                } else {
                    Vec::new()
                };
                data.resize(lease.len as usize, 0);
                LeaseBuffer {
                    attributes,
                    buffer: Buffer::new(data),
                }
            })
            .collect();
        let entries = leases
            .iter()
            .map(|lease| ULease {
                attributes: lease.attributes,
                base_address: Addr::new(lease.buffer.address),
                length: lease.buffer.data.len(),
            })
            .collect();
        CallKind::Send {
            target: TaskId(r.target),
            operation: r.operation,
            message: Buffer::new(r.message),
            reply: Buffer::zeroed(r.reply_capacity as usize),
            leases,
            lease_table: LeaseTable::new(entries),
        }
    } else if key == <syscall_methods::recv as Method>::KEY {
        let r: RecvRequest = parse(body)?;
        CallKind::Recv {
            buffer: Buffer::zeroed(r.capacity as usize),
            notification_mask: r.notification_mask,
            specific_sender: r.specific_sender.map(TaskId),
        }
    } else if key == <syscall_methods::reply as Method>::KEY {
        let r: ReplyRequest = parse(body)?;
        CallKind::Reply {
            peer: TaskId(r.peer),
            code: r.code,
            message: Buffer::new(r.message),
        }
    } else if key == <syscall_methods::set_timer as Method>::KEY {
        let r: SetTimerRequest = parse(body)?;
        CallKind::SetTimer {
            deadline: r.deadline,
            notifications: r.notifications,
        }
    } else if key == <syscall_methods::borrow_read as Method>::KEY {
        let r: BorrowReadRequest = parse(body)?;
        CallKind::BorrowRead {
            lender: TaskId(r.lender),
            index: r.index as usize,
            offset: r.offset as usize,
            buffer: Buffer::zeroed(r.len as usize),
        }
    } else if key == <syscall_methods::borrow_write as Method>::KEY {
        let r: BorrowWriteRequest = parse(body)?;
        CallKind::BorrowWrite {
            lender: TaskId(r.lender),
            index: r.index as usize,
            offset: r.offset as usize,
            data: Buffer::new(r.data),
        }
    } else if key == <syscall_methods::borrow_info as Method>::KEY {
        let r: BorrowInfoRequest = parse(body)?;
        CallKind::BorrowInfo {
            lender: TaskId(r.lender),
            index: r.index as usize,
        }
    } else if key == <syscall_methods::irq_control as Method>::KEY {
        let r: IrqControlRequest = parse(body)?;
        CallKind::IrqControl {
            mask: r.mask,
            flags: r.flags,
        }
    } else if key == <syscall_methods::panic as Method>::KEY {
        let r: PanicRequest = parse(body)?;
        CallKind::Panic {
            message: Buffer::new(r.message),
        }
    } else if key == <syscall_methods::get_timer as Method>::KEY {
        let () = parse(body)?;
        CallKind::GetTimer
    } else if key == <syscall_methods::refresh_task_id as Method>::KEY {
        let id: u16 = parse(body)?;
        CallKind::RefreshTaskId {
            task_id: TaskId(id),
        }
    } else if key == <syscall_methods::post as Method>::KEY {
        let r: PostRequest = parse(body)?;
        CallKind::Post {
            task: TaskId(r.task),
            bits: r.bits,
        }
    } else if key == <syscall_methods::reply_fault as Method>::KEY {
        let r: ReplyFaultRequest = parse(body)?;
        CallKind::ReplyFault {
            peer: TaskId(r.peer),
            reason: r.reason,
        }
    } else if key == <syscall_methods::irq_status as Method>::KEY {
        let mask: u32 = parse(body)?;
        CallKind::IrqStatus { mask }
    } else if key == <runtime_methods::task_slot as Method>::KEY {
        let name: String = parse(body)?;
        return Ok(Incoming::TaskSlot { header, name });
    } else {
        return Err(format!("unknown method {key:?}"));
    };

    Ok(Incoming::Syscall(Call {
        header,
        kind,
        result: None,
    }))
}

/// Serializes a response frame: the request's header with the operation
/// flipped, followed by the body.
fn frame<T: Serialize>(request: &Header, body: &T) -> Vec<u8> {
    let header = Header {
        op: Operation::Response,
        ..request.clone()
    };
    let mut out = postcard::to_stdvec(&header)
        .unwrap_or_else(|e| fatal(format_args!("encoding a header: {e}")));
    out.extend(
        postcard::to_stdvec(body).unwrap_or_else(|e| {
            fatal(format_args!("encoding a response: {e}"))
        }),
    );
    out
}

/// Builds the response to a completed syscall from the captured result and
/// the buffers the kernel may have written.
fn encode_response(call: &Call) -> Vec<u8> {
    let header = &call.header;
    let result = call.result;
    match &call.kind {
        CallKind::Send { reply, leases, .. } => {
            let (code, len) = match result {
                Some(CallResult::Send { code, len }) => (code, len),
                Some(CallResult::Error(code)) => (code, 0),
                _ => (0, 0),
            };
            frame(
                header,
                &Ok::<_, Fault>(SendResponse {
                    code,
                    reply: reply.prefix(len),
                    lease_writebacks: leases
                        .iter()
                        .map(|lease| {
                            lease
                                .attributes
                                .contains(LeaseAttributes::WRITE)
                                .then(|| lease.buffer.data.clone())
                        })
                        .collect(),
                }),
            )
        }
        CallKind::Recv { buffer, .. } => {
            let response: RecvResponse = match result {
                Some(CallResult::Recv {
                    sender,
                    operation,
                    len,
                    response_capacity,
                    lease_count,
                }) => Ok(RecvMessage {
                    sender: sender.0,
                    operation,
                    message_len: len as u32,
                    message: buffer.prefix(len),
                    response_capacity: response_capacity as u32,
                    lease_count: lease_count as u32,
                }),
                Some(CallResult::Error(code)) => Err(code),
                _ => Err(0),
            };
            frame(header, &Ok::<_, Fault>(response))
        }
        CallKind::Reply { .. }
        | CallKind::SetTimer { .. }
        | CallKind::IrqControl { .. }
        | CallKind::ReplyFault { .. } => frame(header, &Ok::<(), Fault>(())),
        CallKind::BorrowRead { buffer, .. } => {
            let (code, len) = match result {
                Some(CallResult::Borrow { code, len }) => (code, len),
                Some(CallResult::Error(code)) => (code, 0),
                _ => (0, 0),
            };
            frame(
                header,
                &Ok::<_, Fault>(BorrowReadResponse {
                    code,
                    data: buffer.prefix(len),
                }),
            )
        }
        CallKind::BorrowWrite { .. } => {
            let (code, len) = match result {
                Some(CallResult::Borrow { code, len }) => (code, len),
                Some(CallResult::Error(code)) => (code, 0),
                _ => (0, 0),
            };
            frame(
                header,
                &Ok::<_, Fault>(BorrowWriteResponse {
                    code,
                    len: len as u32,
                }),
            )
        }
        CallKind::BorrowInfo { .. } => {
            let info = match result {
                Some(CallResult::BorrowInfo { attributes, len }) => {
                    Some(BorrowInfo {
                        attributes,
                        len: len as u32,
                    })
                }
                _ => None,
            };
            frame(header, &Ok::<_, Fault>(info))
        }
        CallKind::Panic { .. } => frame(header, &()),
        CallKind::GetTimer => {
            let state = match result {
                Some(CallResult::Time {
                    now,
                    deadline,
                    on_deadline,
                }) => TimerState {
                    now,
                    deadline,
                    on_deadline,
                },
                _ => TimerState {
                    now: 0,
                    deadline: None,
                    on_deadline: 0,
                },
            };
            frame(header, &Ok::<_, Fault>(state))
        }
        CallKind::RefreshTaskId { task_id } => {
            let id = match result {
                Some(CallResult::TaskId(id)) => id,
                _ => *task_id,
            };
            frame(header, &Ok::<_, Fault>(id.0))
        }
        CallKind::Post { .. } => {
            let code = match result {
                Some(CallResult::Error(code)) => code,
                _ => 0,
            };
            frame(header, &Ok::<_, Fault>(code))
        }
        CallKind::IrqStatus { .. } => {
            let bits = match result {
                Some(CallResult::IrqStatus(bits)) => bits,
                _ => 0,
            };
            frame(header, &Ok::<_, Fault>(bits))
        }
    }
}

/////////////////////////////////////////////////////////////////////////////
// The scheduler loop

/// The task the kernel will resume next; set by `set_current_task`, exactly
/// as on ARM, and read only between kernel entries.
static CURRENT_TASK_PTR: AtomicPtr<Task> =
    AtomicPtr::new(core::ptr::null_mut());

/// Virtual time, in ticks.
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Software interrupts pended by `pend_software_irq`, delivered at the next
/// scheduling point.
static PENDING_IRQS: Mutex<Vec<InterruptNum>> = Mutex::new(Vec::new());

/// Interrupts a task has enabled, so `irq_status` can report them.
static ENABLED_IRQS: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

fn current_task_index() -> (*mut Task, usize) {
    let current = CURRENT_TASK_PTR.load(Ordering::Relaxed);
    if current.is_null() {
        fatal("no current task");
    }
    // Safety: the pointer came from a `&mut Task` in the task table (see
    // `set_current_task`), and this is only called between kernel entries,
    // when no other reference into the table is live.
    let index = unsafe { usize::from((*current).descriptor().index) };
    (current, index)
}

/// One trip through the scheduler: resume the current task, wait for its
/// next syscall, and run the kernel for it.
fn step() {
    let (current, index) = current_task_index();
    let cfg = config();

    if cfg.tasks.get(index).is_some_and(|t| t.idle) {
        idle_step(index);
        return;
    }

    // Safety: as in `current_task_index`; the reference is dropped before
    // any other kernel entry below.
    let finished_call = unsafe { (*current).save_mut().call.take() };

    let incoming = {
        let mut procs = processes();
        let Some(proc) = procs.get_mut(index).and_then(Option::as_mut) else {
            fatal(format_args!("task {index} has no process to resume"))
        };
        if let Some(call) = finished_call {
            trace_task!(
                &[index],
                "task {}: resumed from {} with {:?}",
                cfg.tasks[index].name,
                sysnum_name(call.kind.sysnum()),
                call.result
            );
            let response = encode_response(&call);
            if let Err(e) = proc.io.send_one_frame_raw(RawIoFrame {
                meta: (),
                raw: &response,
            }) {
                trace!("resuming task {index} failed: {e:?}");
            }
        }
        read_incoming(proc)
    };

    match incoming {
        Incoming::TaskSlot { header, name } => {
            // Not a syscall: answered here, and the task keeps running.
            crate::profiling::event_secondary_syscall_enter();
            let task = &cfg.tasks[index];
            let answer = match task.slots.get(&name) {
                Some(&slot_index) => Ok::<u16, Fault>(slot_index),
                None => Err(Fault {
                    description: format!(
                        "task {} has no task slot named {name:?}",
                        task.name
                    ),
                }),
            };
            trace_task!(
                &[index],
                "task {}: task_slot {name:?} -> {answer:?}",
                task.name
            );
            let response = frame(&header, &answer);
            if let Some(proc) =
                processes().get_mut(index).and_then(Option::as_mut)
            {
                proc.io
                    .send_one_frame_raw(RawIoFrame {
                        meta: (),
                        raw: &response,
                    })
                    .ok();
            }
            crate::profiling::event_secondary_syscall_exit();
        }
        Incoming::Syscall(call) => {
            if tracing() {
                let (what, peer) = describe_syscall(index, &call.kind);
                let participants: Vec<usize> =
                    [Some(index), peer].into_iter().flatten().collect();
                trace_task!(
                    &participants,
                    "task {}: {what}",
                    cfg.tasks[index].name
                );
            }
            let nr = call.kind.sysnum() as u32;
            // Safety: as in `current_task_index`.
            unsafe { (*current).save_mut().call = Some(call) };
            // Safety: `current` is a valid, initialized entry of the task
            // table and no reference into the table is live.
            unsafe { crate::syscalls::syscall_entry(nr, current) };
        }
        Incoming::Gone(fault) => {
            trace_task!(
                &[index],
                "task {}: process ended, {fault:?}",
                cfg.tasks[index].name
            );
            with_task_table(|tasks| {
                let _ = task::force_fault(tasks, index, fault);
                let next = task::select(index, tasks);
                // Safety: `next` is in the task table.
                unsafe { next.switch_to() }
            });
        }
    }

    deliver_pending_irqs();
}

/// The idle task was selected: nothing can run until a timer fires. Advance
/// virtual time to the next deadline, or stop if there is none.
fn idle_step(idle_index: usize) {
    if deliver_pending_irqs() {
        return;
    }
    with_task_table(|tasks| {
        let next_deadline = tasks.iter().filter_map(|t| t.timer().0).min();
        let Some(deadline) = next_deadline else {
            finish("every task is blocked and no timer is pending")
        };
        let target = deadline.max(now());
        if let Some(stop) = config().stop_at
            && u64::from(target) > stop
        {
            finish(format_args!(
                "virtual time reached the configured stop at {stop} ticks"
            ))
        }
        TICKS.store(target.into(), Ordering::Relaxed);
        // Not attributable to a task: shown only when tracing everything.
        trace_task!(&[], "idle: advanced time to the next deadline");
        crate::profiling::event_timer_isr_enter();
        let _ = task::process_timers(tasks, target);
        crate::profiling::event_timer_isr_exit();
        let next = task::select(idle_index, tasks);
        // Safety: `next` is in the task table.
        unsafe { next.switch_to() }
    });
}

/// Posts any software-pended interrupts to their owners. Returns whether
/// that changed which task is current.
fn deliver_pending_irqs() -> bool {
    let irqs = std::mem::take(
        &mut *PENDING_IRQS.lock().unwrap_or_else(|e| e.into_inner()),
    );
    if irqs.is_empty() {
        return false;
    }
    let (_, index) = current_task_index();
    crate::profiling::event_isr_enter();
    let switched = with_task_table(|tasks| {
        let mut woke = false;
        for irq in irqs {
            let Some(owner) = crate::startup::HUBRIS_IRQ_TASK_LOOKUP.get(irq)
            else {
                continue;
            };
            // As on hardware, an interrupt is masked once delivered until the
            // task re-enables it.
            disable_irq(irq.0, false).ok();
            woke |= tasks[owner.task as usize]
                .post(NotificationSet(owner.notification));
        }
        if woke {
            let next = task::select(index, tasks);
            // Safety: `next` is in the task table.
            unsafe { next.switch_to() }
        }
        woke
    });
    crate::profiling::event_isr_exit();
    switched
}

/////////////////////////////////////////////////////////////////////////////
// The arch interface

/// Region descriptors carry no host-specific data.
#[derive(Copy, Clone, Debug)]
pub struct RegionDescExt;

pub const fn compute_region_extension_data(
    _base: usize,
    _size: usize,
    _attributes: RegionAttributes,
) -> RegionDescExt {
    RegionDescExt
}

/// Loads the run configuration, so a misconfigured run fails before any task
/// starts. The tick divisor is meaningless with virtual time.
pub unsafe fn set_clock_freq(_tick_divisor: u32) {
    let _ = config();
}

pub fn reinitialize(task: &mut Task) {
    *task.save_mut() = SavedState::default();
    spawn_task(usize::from(task.descriptor().index));
}

/// Nothing to do: the kernel never dereferences task addresses, and task
/// processes are isolated by the host already.
pub fn apply_memory_protection(_task: &Task) {}

pub fn start_first_task(_tick_divisor: u32, task: &mut Task) -> ! {
    // Safety: `task` is in the task table, per `start_kernel`.
    unsafe { set_current_task(task) };
    loop {
        step();
    }
}

/// Records `task` as the one to resume next.
///
/// # Safety
///
/// `task` must be an entry of the task table, and the caller must not keep
/// using the reference once the kernel returns to the scheduler loop.
pub unsafe fn set_current_task(task: &mut Task) {
    let task: *mut Task = task;
    CURRENT_TASK_PTR.store(task, Ordering::Relaxed);
    crate::profiling::event_context_switch(task as usize);
}

pub fn now() -> Timestamp {
    Timestamp::from(TICKS.load(Ordering::Relaxed))
}

pub fn disable_irq(
    n: u32,
    _also_clear_pending: bool,
) -> Result<(), UsageError> {
    ENABLED_IRQS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&n);
    Ok(())
}

pub fn enable_irq(n: u32, _also_clear_pending: bool) -> Result<(), UsageError> {
    ENABLED_IRQS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(n);
    Ok(())
}

pub fn irq_status(n: u32) -> Result<IrqStatus, UsageError> {
    let mut status = IrqStatus::empty();
    let enabled = ENABLED_IRQS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&n);
    status.set(IrqStatus::ENABLED, enabled);
    let pending = PENDING_IRQS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&InterruptNum(n));
    status.set(IrqStatus::PENDING, pending);
    Ok(status)
}

pub fn pend_software_irq(irq: InterruptNum) -> Result<(), UsageError> {
    if crate::startup::HUBRIS_IRQ_TASK_LOOKUP.get(irq).is_none() {
        return Err(UsageError::NoIrq);
    }
    PENDING_IRQS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(irq);
    Ok(())
}

pub fn reset() -> ! {
    finish("the supervisor asked for a system reset")
}

impl AtomicExt for AtomicBool {
    type Primitive = bool;

    #[inline(always)]
    fn swap_polyfill(&self, value: bool, ordering: Ordering) -> bool {
        self.swap(value, ordering)
    }
}
