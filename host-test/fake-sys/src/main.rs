// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Fake `drv-stm32xx-sys` for host tests.
//! robo:
//! robo: Models GPIO pins and EXTI pin interrupts; clock and reset
//! robo: operations are accepted and ignored. The orchestrator sets the
//! robo: level the outside world puts on each pin and inspects pin state and
//! robo: a log of operations through [`host_test_api::sys`].
//! robo:
//! robo: Pins start in analog mode, which reads as 0, as on an STM32 after
//! robo: reset.

use std::collections::BTreeMap;

use drv_stm32xx_sys_api::{Edge, IrqControl, Port, RccError};
use host_test_api::control::{Control, serve_one};
use host_test_api::sys::{
    Event, EventKind, Mode, Pin, PinState, Request, Response,
};
use idol_runtime::{ClientError, RequestError};
use userlib::{RecvMessage, TaskId, sys_get_timer, sys_post};

/// robo: Most events returned per control reply.
const EVENTS_PER_REPLY: usize = 256;

/// robo: Number of EXTI pin lines, one per pin number.
const NUM_EXTI: usize = 16;

#[derive(Copy, Clone, Default)]
struct PortState {
    /// robo: Mode of each pin, from `gpio_configure_raw`.
    modes: [Option<Mode>; 16],
    /// robo: Open-drain bit of each pin.
    open_drain: u16,
    /// robo: Output data register.
    odr: u16,
    /// robo: Levels the outside world puts on the pins.
    external: u16,
}

impl PortState {
    fn pin(&self, pin: u8) -> PinState {
        let bit = 1 << pin;
        PinState {
            mode: self.modes[usize::from(pin)].unwrap_or(Mode::Analog),
            open_drain: self.open_drain & bit != 0,
            odr: self.odr & bit != 0,
            external: self.external & bit != 0,
        }
    }

    /// robo: What the input data register reads.
    fn read(&self) -> u16 {
        (0..16).fold(0, |acc, pin| {
            let p = self.pin(pin);
            if p.mode != Mode::Analog && p.level() {
                acc | 1 << pin
            } else {
                acc
            }
        })
    }

    /// robo: Levels on the nets, for edge detection.
    fn levels(&self) -> u16 {
        (0..16).fold(0, |acc, pin| {
            if self.pin(pin).level() {
                acc | 1 << pin
            } else {
                acc
            }
        })
    }
}

struct ServerImpl {
    ports: BTreeMap<u8, PortState>,
    /// robo: EXTI lines that trigger on a rising edge.
    rising: u16,
    /// robo: EXTI lines that trigger on a falling edge.
    falling: u16,
    /// robo: EXTI lines that are unmasked.
    enabled: u16,
    /// robo: EXTI lines that fired since their owner last checked.
    pending: u16,
    events: Vec<Event>,
}

impl ServerImpl {
    fn log(&mut self, kind: EventKind) {
        self.events.push(Event {
            time: sys_get_timer().now,
            kind,
        });
    }

    fn port(&mut self, port: u8) -> &mut PortState {
        self.ports.entry(port).or_default()
    }

    /// robo: Runs `f` on `port`, then fires any EXTI lines whose pin level
    /// robo: changed in a direction they're sensitive to.
    fn update_port(&mut self, port: u8, f: impl FnOnce(&mut PortState)) {
        let before = self.port(port).levels();
        f(self.port(port));
        let after = self.port(port).levels();

        let changed = before ^ after;
        for line in 0..NUM_EXTI {
            let bit = 1 << line;
            if changed & bit == 0 || self.enabled & bit == 0 {
                continue;
            }
            let Some(entry) = &generated::EXTI_DISPATCH_TABLE[line] else {
                continue;
            };
            if entry.port as u8 != port {
                continue;
            }
            let rose = after & bit != 0;
            let sensitive = if rose { self.rising } else { self.falling };
            if sensitive & bit == 0 {
                continue;
            }
            // robo: Same as the real sys task: post, mask the line until its
            // robo: owner re-enables it, and latch it as pending.
            let task = userlib::sys_refresh_task_id(entry.task);
            sys_post(task, entry.mask);
            self.enabled &= !bit;
            self.pending |= bit;
            self.log(EventKind::Posted {
                task: task.index() as u16,
                mask: entry.mask,
            });
        }
    }
}

/// robo: EXTI lines mapped to notification bits in `mask` of `task`.
fn exti_lines_for(task: TaskId, mask: u32) -> u16 {
    let mut lines = 0;
    for (line, entry) in generated::EXTI_DISPATCH_TABLE.iter().enumerate() {
        if let Some(entry) = entry
            && entry.task.index() == task.index()
            && entry.mask & mask != 0
        {
            lines |= 1 << line;
        }
    }
    lines
}

/// robo: Unpacks the mode from `gpio_configure_raw`'s packed attributes.
fn unpack_mode(packed: u16) -> Mode {
    match packed & 0b11 {
        0b00 => Mode::Input,
        0b01 => Mode::Output,
        0b10 => Mode::Alternate,
        _ => Mode::Analog,
    }
}

/// robo: Unpacks the open-drain bit from `gpio_configure_raw`'s packed
/// robo: attributes.
fn unpack_open_drain(packed: u16) -> bool {
    packed & 0b100 != 0
}

impl idl::InOrderSysImpl for ServerImpl {
    fn enable_clock_raw(
        &mut self,
        _: &RecvMessage,
        _raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        Ok(())
    }

    fn disable_clock_raw(
        &mut self,
        _: &RecvMessage,
        _raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        Ok(())
    }

    fn enter_reset_raw(
        &mut self,
        _: &RecvMessage,
        _raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        Ok(())
    }

    fn leave_reset_raw(
        &mut self,
        _: &RecvMessage,
        _raw: u32,
    ) -> Result<(), RequestError<RccError>> {
        Ok(())
    }

    fn gpio_configure_raw(
        &mut self,
        rm: &RecvMessage,
        port: Port,
        pins: u16,
        packed_attributes: u16,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        let mode = unpack_mode(packed_attributes);
        let open_drain = unpack_open_drain(packed_attributes);
        self.update_port(port as u8, |p| {
            for pin in 0..16 {
                if pins & 1 << pin != 0 {
                    p.modes[pin] = Some(mode);
                }
            }
            if open_drain {
                p.open_drain |= pins;
            } else {
                p.open_drain &= !pins;
            }
        });
        self.log(EventKind::Configure {
            task: rm.sender.index() as u16,
            port: port as u8,
            pins,
            mode,
        });
        Ok(())
    }

    fn gpio_set_reset(
        &mut self,
        rm: &RecvMessage,
        port: Port,
        set_pins: u16,
        reset_pins: u16,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        // robo: As with BSRR, set wins if a pin is in both masks.
        self.update_port(port as u8, |p| {
            p.odr = (p.odr & !reset_pins) | set_pins;
        });
        self.log(EventKind::SetReset {
            task: rm.sender.index() as u16,
            port: port as u8,
            set: set_pins,
            reset: reset_pins,
        });
        Ok(())
    }

    fn gpio_toggle(
        &mut self,
        _: &RecvMessage,
        port: Port,
        pins: u16,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        self.update_port(port as u8, |p| p.odr ^= pins);
        Ok(())
    }

    fn gpio_read_input(
        &mut self,
        _: &RecvMessage,
        port: Port,
    ) -> Result<u16, RequestError<core::convert::Infallible>> {
        Ok(self.port(port as u8).read())
    }

    fn read_uid(
        &mut self,
        _: &RecvMessage,
    ) -> Result<[u32; 3], RequestError<core::convert::Infallible>> {
        Ok([0x4f58_4944, 0x484f_5354, 0x5445_5354])
    }

    fn gpio_irq_configure(
        &mut self,
        rm: &RecvMessage,
        mask: u32,
        sensitivity: Edge,
    ) -> Result<(), RequestError<core::convert::Infallible>> {
        let lines = exti_lines_for(rm.sender, mask);
        if lines == 0 {
            return Err(ClientError::BadMessageContents.fail());
        }
        if sensitivity.is_rising() {
            self.rising |= lines;
        } else {
            self.rising &= !lines;
        }
        if sensitivity.is_falling() {
            self.falling |= lines;
        } else {
            self.falling &= !lines;
        }
        Ok(())
    }

    fn gpio_irq_control(
        &mut self,
        rm: &RecvMessage,
        mask: u32,
        op: IrqControl,
    ) -> Result<bool, RequestError<core::convert::Infallible>> {
        let lines = exti_lines_for(rm.sender, mask);
        if lines == 0 {
            return Err(ClientError::BadMessageContents.fail());
        }
        match op {
            IrqControl::Enable => self.enabled |= lines,
            IrqControl::Disable => self.enabled &= !lines,
            IrqControl::Check => {}
        }
        let pending = self.pending & lines != 0;
        self.pending &= !lines;
        Ok(pending)
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

impl Control for ServerImpl {
    type Request = Request;
    type Response = Response;

    fn control(&mut self, req: Request) -> Response {
        match req {
            Request::SetExternal {
                pin: Pin { port, pin },
                high,
            } => {
                self.update_port(port, |p| {
                    if high {
                        p.external |= 1 << pin;
                    } else {
                        p.external &= !(1 << pin);
                    }
                });
                self.log(EventKind::External {
                    pin: Pin { port, pin },
                    high,
                });
                Response::Done
            }
            Request::Pin(Pin { port, pin }) => {
                Response::Pin(self.port(port).pin(pin))
            }
            Request::Events { from } => Response::Events(
                self.events
                    .iter()
                    .skip(from)
                    .take(EVENTS_PER_REPLY)
                    .cloned()
                    .collect(),
            ),
        }
    }
}

/// robo: Receive buffer size: fits any operation of the real API, and
/// robo: control requests.
const BUFFER_SIZE: usize = if idl::INCOMING_SIZE > 1024 {
    idl::INCOMING_SIZE
} else {
    1024
};

fn main() -> ! {
    let mut server = ServerImpl {
        ports: BTreeMap::new(),
        rising: 0,
        falling: 0,
        enabled: 0,
        pending: 0,
        events: Vec::new(),
    };
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        serve_one::<_, idl::SysOperation>(&mut buffer, &mut server);
    }
}

/// robo: Matches the struct the generated EXTI table names. Copied from
/// robo: `drv-stm32xx-sys`.
struct ExtiDispatch {
    port: Port,
    task: TaskId,
    mask: u32,
    #[allow(dead_code)]
    name: generated::ExtiIrq,
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

mod idl {
    use super::{Edge, IrqControl, Port, RccError};

    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

mod generated {
    use super::*;

    include!(concat!(env!("OUT_DIR"), "/exti_config.rs"));
}
