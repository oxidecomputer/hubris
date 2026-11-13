// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! robo: Control interface of `fake-packrat`.

use serde::{Deserialize, Serialize};
use userlib::TaskId;

use crate::control::call;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Ereports { from: usize },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Ereports(Vec<Ereport>),
}

/// robo: One delivered ereport, as the CBOR bytes the task sent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ereport {
    pub time: u64,
    pub task: u16,
    pub bytes: Vec<u8>,
}

impl Ereport {
    /// robo: The ereport decoded into a JSON-like tree.
    pub fn decode(&self) -> serde_json::Value {
        minicbor_serde::from_slice(&self.bytes).unwrap_or_else(|e| {
            panic!("ereport at t={} is not valid CBOR: {e}", self.time)
        })
    }

    /// robo: The ereport's class (its `"k"` field).
    pub fn class(&self) -> String {
        match self.decode().get("k") {
            Some(serde_json::Value::String(k)) => k.clone(),
            other => panic!("ereport has no string class: {other:?}"),
        }
    }

    /// robo: Field `name` of the ereport.
    pub fn field(&self, name: &str) -> serde_json::Value {
        self.decode()
            .get(name)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }
}

/// robo: Client for fake-packrat's control interface.
#[derive(Copy, Clone)]
pub struct FakePackrat(pub TaskId);

impl FakePackrat {
    /// robo: Ereports delivered so far, starting at index `from`.
    pub fn ereports_from(&self, from: usize) -> Vec<Ereport> {
        let mut all = Vec::new();
        loop {
            let req = Request::Ereports {
                from: from + all.len(),
            };
            match call(self.0, &req) {
                Response::Ereports(e) if e.is_empty() => return all,
                Response::Ereports(e) => all.extend(e),
            }
        }
    }

    /// robo: Every ereport delivered so far.
    pub fn ereports(&self) -> Vec<Ereport> {
        self.ereports_from(0)
    }
}
