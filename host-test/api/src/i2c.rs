// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Control interface of `fake-i2c`.
//! robo:
//! robo: Each attached device is a register file: a write of `[cmd, data..]`
//! robo: stores `data` in register `cmd`, and a read after writing `[cmd]`
//! robo: returns the stored bytes. That covers SMBus/PMBus devices that only
//! robo: need plain reads, block reads, and writes. Device-specific behavior
//! robo: (e.g. a PSU turning off) is the scenario's job.

use drv_i2c_api::{I2cDevice, Marshal};
use serde::{Deserialize, Serialize};
use userlib::TaskId;

use crate::control::call;

/// robo: A device address, as the 4-byte header `drv-i2c-api` sends:
/// robo: address, controller, port, mux/segment.
pub type Addr = [u8; 4];

/// robo: The [`Addr`] of `dev`.
pub fn addr_of(dev: &I2cDevice) -> Addr {
    Marshal::marshal(&(dev.address, dev.controller, dev.port, dev.segment))
}

/// robo: Register contents: command code and bytes.
pub type Register = (u8, Vec<u8>);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Attach { addr: Addr, regs: Vec<Register> },
    Detach { addr: Addr },
    SetRegister { addr: Addr, reg: Register },
    Events { from: usize },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Done,
    Events(Vec<Event>),
}

/// robo: One logged transfer (one write/read lease pair).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub time: u64,
    pub task: u16,
    pub addr: Addr,
    pub write: Vec<u8>,
    pub read_len: usize,
    /// robo: False if the device was absent or lacked the register.
    pub ok: bool,
}

/// robo: Client for fake-i2c's control interface.
#[derive(Copy, Clone)]
pub struct FakeI2c(pub TaskId);

impl FakeI2c {
    fn done(&self, req: Request) {
        match call(self.0, &req) {
            Response::Done => {}
            r => panic!("unexpected reply {r:?}"),
        }
    }

    /// robo: Makes a device answer at `addr`, replacing any already there.
    pub fn attach(&self, addr: Addr, regs: Vec<Register>) {
        self.done(Request::Attach { addr, regs })
    }

    /// robo: Makes `addr` NACK.
    pub fn detach(&self, addr: Addr) {
        self.done(Request::Detach { addr })
    }

    pub fn set_register(&self, addr: Addr, cmd: u8, bytes: Vec<u8>) {
        self.done(Request::SetRegister {
            addr,
            reg: (cmd, bytes),
        })
    }

    /// robo: Every logged transfer so far.
    pub fn events(&self) -> Vec<Event> {
        let mut all = Vec::new();
        loop {
            match call(self.0, &Request::Events { from: all.len() }) {
                Response::Events(e) if e.is_empty() => return all,
                Response::Events(e) => all.extend(e),
                r => panic!("unexpected reply {r:?}"),
            }
        }
    }
}

/// robo: Register presets for the Murata MWOCP68.
pub mod mwocp68 {
    use super::Register;
    use pmbus::commands::mwocp68::CommandCode;

    /// robo: Registers a healthy MWOCP68 with serial number `serial`
    /// robo: (at most 12 bytes) answers with.
    pub fn healthy(serial: &str) -> Vec<Register> {
        let block = |c: CommandCode, s: &str| (c as u8, s.as_bytes().to_vec());
        let byte = |c: CommandCode| (c as u8, vec![0]);
        vec![
            block(CommandCode::MFR_ID, "Murata-PS"),
            block(CommandCode::MFR_MODEL, "MWOCP68-3600-D-RM"),
            block(CommandCode::MFR_SERIAL, serial),
            block(CommandCode::MFR_REVISION, "0701-0702-0000"),
            (CommandCode::STATUS_WORD as u8, vec![0, 0]),
            byte(CommandCode::STATUS_VOUT),
            byte(CommandCode::STATUS_IOUT),
            byte(CommandCode::STATUS_INPUT),
            byte(CommandCode::STATUS_CML),
            byte(CommandCode::STATUS_TEMPERATURE),
            byte(CommandCode::STATUS_MFR_SPECIFIC),
        ]
    }
}
