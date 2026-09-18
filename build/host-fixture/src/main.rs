// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A small fixture for running one host-built Hubris task.
//!
//! The fixture launches the task executable with its stdin and stdout piped,
//! and then plays the kernel and every other task for it, answering each
//! syscall it makes over the `hostcall` protocol. This example is just enough
//! to exercise `task/ping` and `task/pong`:
//!
//! * Sends to the task in the `user_leds` slot are answered like the user LED
//!   driver would: LED indices below four succeed, others get `NotPresent`.
//! * Sends to the task in the `usart_driver` slot succeed, and the leased
//!   text is echoed to the log.
//! * Sends to any other task get an empty reply with a counting response
//!   code, which is what `pong` sends back to `ping`.
//! * Receives are answered with a `ping`-style message from a fake peer, or,
//!   once a few of those have been delivered, by firing the task's timer,
//!   which advances the virtual clock to the deadline.
//! * Leases, interrupts and posts are logged but not modelled.
//!
//! Every syscall is logged to stderr, and the task's own stderr is passed
//! through, so the trace of a run reads as one conversation. Build a task with
//! `cargo xtask host-build <app.toml> <task>` and pass the resulting path.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use clap::Parser;
use hostcall::nprpc::{Request, ServerIoError};
use hostcall::{
    BorrowInfo, BorrowInfoRequest, BorrowReadRequest, BorrowReadResponse,
    BorrowWriteRequest, BorrowWriteResponse, Fault, IoError, IrqControlRequest,
    KERNEL_TASK_ID, Outcome, PanicRequest, PostRequest, RecvMessage,
    RecvRequest, RecvResponse, ReplyFaultRequest, ReplyRequest, SendRequest,
    SendResponse, Server, SetTimerRequest, TimerState, all, runtime, syscalls,
};

#[derive(Parser)]
#[clap(about = "Runs a host-built Hubris task, playing the kernel for it")]
struct Args {
    /// Path to a task executable built by `cargo xtask host-build`.
    task: PathBuf,

    /// A task slot the task may ask for, as NAME=INDEX. May be repeated.
    /// Defaults to the slots ping and pong use: peer=2, user_leds=3,
    /// usart_driver=4.
    #[clap(long = "slot", value_name = "NAME=INDEX")]
    slots: Vec<String>,

    /// Stop the task after this many syscalls.
    #[clap(long, default_value = "300")]
    max_syscalls: u64,

    /// Response code for the first send to a counting peer; each further send
    /// gets the next code. (ping faults itself on every hundredth code, so
    /// starting at 0 would fault it immediately, as it does on hardware.)
    #[clap(long, default_value = "1")]
    first_code: u32,

    /// Number of receives answered with a message before a pending timer is
    /// allowed to fire.
    #[clap(long, default_value = "2")]
    messages_per_timer: u32,
}

/// Task index used as the sender of synthesized messages.
const FAKE_PEER: u16 = 1;

struct Fixture {
    slots: BTreeMap<String, u16>,
    /// Virtual time in ticks; only advances when a timer fires.
    now: u64,
    timer: Option<(u64, u32)>,
    next_code: u32,
    recvs_since_timer: u32,
    messages_per_timer: u32,
    syscalls: u64,
}

impl Fixture {
    fn log(&mut self, what: impl std::fmt::Display) {
        self.syscalls += 1;
        eprintln!("[fixture #{:<4} t={:<6}] {what}", self.syscalls, self.now);
    }

    fn slot_named(&self, name: &str) -> Option<u16> {
        self.slots.get(name).copied()
    }

    fn led_reply(&mut self, rqst: &SendRequest) -> SendResponse {
        let op = match rqst.operation {
            1 => "led_on",
            2 => "led_off",
            3 => "led_toggle",
            4 => "led_blink",
            _ => "unknown LED op",
        };
        // The argument is `index: usize`, laid out for the host; accept the
        // 32-bit layout too in case the task was built for a 32-bit host.
        let index = match rqst.message.len() {
            8 => u64::from_le_bytes(rqst.message[..8].try_into().unwrap()),
            4 => {
                u32::from_le_bytes(rqst.message[..4].try_into().unwrap()).into()
            }
            n => {
                self.log(format!("  {op} with a {n}-byte argument: bad size"));
                return SendResponse {
                    code: 2,
                    reply: Vec::new(),
                    lease_writebacks: vec![None; rqst.leases.len()],
                };
            }
        };
        let code = if index < 4 { 0 } else { 1 };
        eprintln!(
            "[fixture      ] user_leds: {op}({index}) -> {}",
            if code == 0 { "Ok" } else { "Err(NotPresent)" }
        );
        SendResponse {
            code,
            reply: Vec::new(),
            lease_writebacks: vec![None; rqst.leases.len()],
        }
    }

    /// The UART driver's write operation: text arrives in the first lease.
    fn uart_reply(&mut self, rqst: &SendRequest) -> SendResponse {
        let text = rqst
            .leases
            .first()
            .map(|lease| String::from_utf8_lossy(&lease.contents).into_owned())
            .unwrap_or_default();
        eprintln!("[fixture      ] usart_driver: write {:?}", text.trim_end());
        SendResponse {
            code: 0,
            reply: Vec::new(),
            lease_writebacks: vec![None; rqst.leases.len()],
        }
    }

    fn fire_timer(&mut self, deadline: u64, bits: u32) -> RecvMessage {
        self.now = self.now.max(deadline);
        self.timer = None;
        self.recvs_since_timer = 0;
        eprintln!("[fixture      ] timer fires: notification bits {bits:#x}");
        RecvMessage {
            sender: KERNEL_TASK_ID,
            operation: bits,
            message_len: 0,
            message: Vec::new(),
            response_capacity: 0,
            lease_count: 0,
        }
    }

    fn synthesized_message(&mut self, from: u16) -> RecvMessage {
        self.recvs_since_timer += 1;
        eprintln!("[fixture      ] delivering a ping from task {from}");
        RecvMessage {
            sender: from,
            operation: 1,
            message_len: 5,
            message: b"hello".to_vec(),
            response_capacity: 16,
            lease_count: 0,
        }
    }
}

impl syscalls::Server for Fixture {
    fn send(
        &mut self,
        rqst: Request<'_, SendRequest>,
    ) -> Outcome<SendResponse> {
        let rqst = rqst.body;
        self.log(format!(
            "send to task {} op {} ({} bytes, {} leases, reply capacity {})",
            rqst.target & 0x3ff,
            rqst.operation,
            rqst.message.len(),
            rqst.leases.len(),
            rqst.reply_capacity
        ));
        if Some(rqst.target & 0x3ff) == self.slot_named("user_leds") {
            return Ok(self.led_reply(rqst));
        }
        if Some(rqst.target & 0x3ff) == self.slot_named("usart_driver") {
            return Ok(self.uart_reply(rqst));
        }
        let code = self.next_code;
        self.next_code = self.next_code.wrapping_add(1);
        eprintln!("[fixture      ] replying with code {code}");
        Ok(SendResponse {
            code,
            reply: Vec::new(),
            lease_writebacks: vec![None; rqst.leases.len()],
        })
    }

    fn recv(
        &mut self,
        rqst: Request<'_, RecvRequest>,
    ) -> Outcome<RecvResponse> {
        let rqst = rqst.body;
        self.log(format!(
            "recv (capacity {}, notification mask {:#x}, from {:?})",
            rqst.capacity, rqst.notification_mask, rqst.specific_sender
        ));
        let timer_ready = self
            .timer
            .filter(|(_, bits)| bits & rqst.notification_mask != 0);
        let message = match rqst.specific_sender {
            Some(KERNEL_TASK_ID) => match timer_ready {
                Some((deadline, bits)) => self.fire_timer(deadline, bits),
                None => {
                    return Err(Fault {
                        description: "closed receive from the kernel with no \
                                      matching timer would block forever"
                            .into(),
                    });
                }
            },
            Some(sender) => self.synthesized_message(sender),
            None => match timer_ready {
                Some((deadline, bits))
                    if self.recvs_since_timer >= self.messages_per_timer =>
                {
                    self.fire_timer(deadline, bits)
                }
                _ => self.synthesized_message(FAKE_PEER),
            },
        };
        Ok(Ok(message))
    }

    fn reply(&mut self, rqst: Request<'_, ReplyRequest>) -> Outcome<()> {
        let rqst = rqst.body;
        self.log(format!(
            "reply to task {} with code {} ({} bytes)",
            rqst.peer & 0x3ff,
            rqst.code,
            rqst.message.len()
        ));
        Ok(())
    }

    fn set_timer(&mut self, rqst: Request<'_, SetTimerRequest>) -> Outcome<()> {
        let rqst = rqst.body;
        self.log(format!(
            "set_timer deadline {:?}, notification bits {:#x}",
            rqst.deadline, rqst.notifications
        ));
        self.timer = rqst.deadline.map(|d| (d, rqst.notifications));
        Ok(())
    }

    fn borrow_read(
        &mut self,
        rqst: Request<'_, BorrowReadRequest>,
    ) -> Outcome<BorrowReadResponse> {
        self.log(format!("borrow_read {:?}", rqst.body));
        Err(Fault {
            description: "this fixture does not model leases".into(),
        })
    }

    fn borrow_write(
        &mut self,
        rqst: Request<'_, BorrowWriteRequest>,
    ) -> Outcome<BorrowWriteResponse> {
        self.log(format!("borrow_write {:?}", rqst.body));
        Err(Fault {
            description: "this fixture does not model leases".into(),
        })
    }

    fn borrow_info(
        &mut self,
        rqst: Request<'_, BorrowInfoRequest>,
    ) -> Outcome<Option<BorrowInfo>> {
        self.log(format!("borrow_info {:?}", rqst.body));
        Ok(None)
    }

    fn irq_control(
        &mut self,
        rqst: Request<'_, IrqControlRequest>,
    ) -> Outcome<()> {
        self.log(format!("irq_control {:?}", rqst.body));
        Ok(())
    }

    fn panic(&mut self, rqst: Request<'_, PanicRequest>) {
        self.log(format!(
            "task panicked: {}",
            String::from_utf8_lossy(&rqst.body.message)
        ));
    }

    fn get_timer(&mut self, _: Request<'_, ()>) -> Outcome<TimerState> {
        self.log("get_timer");
        Ok(TimerState {
            now: self.now,
            deadline: self.timer.map(|(d, _)| d),
            on_deadline: self.timer.map(|(_, bits)| bits).unwrap_or(0),
        })
    }

    fn refresh_task_id(&mut self, rqst: Request<'_, u16>) -> Outcome<u16> {
        self.log(format!("refresh_task_id {:#x}", rqst.body));
        // Nothing ever restarts here, so generation 0 stays current.
        Ok(*rqst.body)
    }

    fn post(&mut self, rqst: Request<'_, PostRequest>) -> Outcome<u32> {
        self.log(format!("post {:?}", rqst.body));
        Ok(0)
    }

    fn reply_fault(
        &mut self,
        rqst: Request<'_, ReplyFaultRequest>,
    ) -> Outcome<()> {
        self.log(format!("reply_fault {:?}", rqst.body));
        Ok(())
    }

    fn irq_status(&mut self, rqst: Request<'_, u32>) -> Outcome<u32> {
        self.log(format!("irq_status {:#x}", rqst.body));
        Ok(0)
    }
}

impl runtime::Server for Fixture {
    fn task_slot(&mut self, rqst: Request<'_, String>) -> Outcome<u16> {
        let name = rqst.body.as_str();
        match self.slot_named(name) {
            Some(index) => {
                self.log(format!("task_slot {name:?} -> {index}"));
                Ok(index)
            }
            None => {
                self.log(format!("task_slot {name:?}: unknown"));
                Err(Fault {
                    description: format!(
                        "task slot {name:?} is not defined; pass --slot {name}=INDEX"
                    ),
                })
            }
        }
    }
}

fn parse_slots(args: &[String]) -> Result<BTreeMap<String, u16>> {
    let mut slots = BTreeMap::from([
        ("peer".to_string(), 2),
        ("user_leds".to_string(), 3),
        ("usart_driver".to_string(), 4),
    ]);
    for arg in args {
        let Some((name, index)) = arg.split_once('=') else {
            bail!("--slot expects NAME=INDEX, got {arg:?}");
        };
        let index = index
            .parse()
            .with_context(|| format!("bad task index in --slot {arg:?}"))?;
        slots.insert(name.to_string(), index);
    }
    Ok(slots)
}

fn describe_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let name = match signal {
                11 => " (SIGSEGV: a memory fault)",
                4 => " (SIGILL: an illegal instruction)",
                8 => " (SIGFPE: an arithmetic fault)",
                9 => " (SIGKILL)",
                _ => "",
            };
            return format!("killed by signal {signal}{name}");
        }
    }
    match status.code() {
        Some(0) => "exited normally, which a task never should".into(),
        Some(hostcall::EXIT_PANIC) => "exited after panicking".into(),
        Some(hostcall::EXIT_FAULT) => "exited after being faulted".into(),
        Some(hostcall::EXIT_TRANSPORT) => {
            "exited after losing the fixture".into()
        }
        Some(code) => format!("exited with status {code}"),
        None => "ended for an unknown reason".into(),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let slots = parse_slots(&args.slots)?;

    let mut child = Command::new(&args.task)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("launching {}", args.task.display()))?;
    let mut backend = Server::for_child(&mut child)
        .expect("child was spawned with piped stdio");

    let mut fixture = Fixture {
        slots,
        now: 0,
        timer: None,
        next_code: args.first_code,
        recvs_since_timer: 0,
        messages_per_timer: args.messages_per_timer,
        syscalls: 0,
    };

    let mut stopped = false;
    loop {
        if fixture.syscalls >= args.max_syscalls {
            eprintln!(
                "[fixture      ] reached {} syscalls, stopping the task",
                args.max_syscalls
            );
            child.kill().context("stopping the task")?;
            stopped = true;
            break;
        }
        match <Fixture as all::Server>::serve_one(&mut fixture, &mut backend) {
            Ok(()) => {}
            Err(ServerIoError::Io(IoError::Eof)) => break,
            Err(e) => {
                eprintln!("[fixture      ] protocol error: {e:?}");
                child.kill().ok();
                break;
            }
        }
    }

    let status = child.wait().context("waiting for the task")?;
    eprintln!(
        "[fixture      ] task {} after {} syscalls at t={}",
        if stopped {
            "was stopped".to_string()
        } else {
            describe_exit(status)
        },
        fixture.syscalls,
        fixture.now
    );
    Ok(())
}
