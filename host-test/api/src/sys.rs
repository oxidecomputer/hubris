// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Control interface of `fake-sys`.

use serde::{Deserialize, Serialize};
use userlib::TaskId;

use crate::control::call;

/// robo: A GPIO pin, by port number (`Port::A` is 0) and pin number.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub port: u8,
    pub pin: u8,
}

/// robo: GPIO pin mode, as in `drv_stm32xx_gpio_common::Mode`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Input,
    Output,
    Alternate,
    Analog,
}

/// robo: What fake-sys knows about one pin.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinState {
    pub mode: Mode,
    /// robo: True for open-drain, false for push-pull.
    pub open_drain: bool,
    /// robo: Output data register bit.
    pub odr: bool,
    /// robo: Level the outside world puts on the pin when we aren't driving
    /// robo: it, as set by [`FakeSys::set_external`].
    pub external: bool,
}

impl PinState {
    /// robo: Whether this pin is driving the net.
    pub fn driving(&self) -> bool {
        self.mode == Mode::Output && (!self.open_drain || !self.odr)
    }

    /// robo: Level on the net.
    pub fn level(&self) -> bool {
        if self.driving() {
            self.odr
        } else {
            self.external
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    SetExternal { pin: Pin, high: bool },
    Pin(Pin),
    Events { from: usize },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Done,
    Pin(PinState),
    Events(Vec<Event>),
}

/// robo: One logged operation, with the kernel time it happened at.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub time: u64,
    pub kind: EventKind,
}

/// robo: Operations fake-sys logs. Reads are not logged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// robo: `gpio_configure_raw` from `task`.
    Configure {
        task: u16,
        port: u8,
        pins: u16,
        mode: Mode,
    },
    /// robo: `gpio_set_reset` from `task`.
    SetReset {
        task: u16,
        port: u8,
        set: u16,
        reset: u16,
    },
    /// robo: The orchestrator changed an external level.
    External { pin: Pin, high: bool },
    /// robo: An EXTI notification posted to `task`.
    Posted { task: u16, mask: u32 },
}

/// robo: Client for fake-sys's control interface.
#[derive(Copy, Clone)]
pub struct FakeSys(pub TaskId);

impl FakeSys {
    fn call(&self, req: Request) -> Response {
        call(self.0, &req)
    }

    /// robo: Sets the level the outside world puts on `pin`. Posts an EXTI
    /// robo: notification if the level changed and the pin's IRQ is armed.
    pub fn set_external(&self, pin: Pin, high: bool) {
        match self.call(Request::SetExternal { pin, high }) {
            Response::Done => {}
            r => panic!("unexpected reply {r:?}"),
        }
    }

    pub fn pin(&self, pin: Pin) -> PinState {
        match self.call(Request::Pin(pin)) {
            Response::Pin(p) => p,
            r => panic!("unexpected reply {r:?}"),
        }
    }

    /// robo: Every logged event so far.
    pub fn events(&self) -> Vec<Event> {
        let mut all = Vec::new();
        loop {
            match self.call(Request::Events { from: all.len() }) {
                Response::Events(e) if e.is_empty() => return all,
                Response::Events(e) => all.extend(e),
                r => panic!("unexpected reply {r:?}"),
            }
        }
    }
}
