// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! # Configuration
//!
//! `HUBRIS_HOST_CONFIG` is the path to a RON file with a [`HostConfig`],
//! produced by `cargo xtask host-run`: one entry per task in task-index order,
//! giving the executable to launch and the task's `task_slot!` name-to-index
//! map, which is passed to the task as an environment variable.
//!
//! Set `HUBRIS_HOST_TRACE` to log syscalls to stderr: `1` (or `all`) logs
//! everything, and a comma-separated list of task names logs only the
//! syscalls made by, or addressed to, those tasks.

use crate::arch::host::fatal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Description of a host run, loaded from the file located at the path in
/// `HUBRIS_HOST_CONFIG`.
#[derive(Debug, Deserialize, Serialize)]
pub struct HostConfig {
    /// Tasks to be managed by the kernel.
    pub tasks: Vec<HostTask>,
    /// Stop the run once virtual time would advance past this tick.
    #[serde(default)]
    pub stop_at: Option<u64>,
    /// Run against the wall clock (one tick per millisecond) instead of
    /// jumping virtual time to the next deadline.
    #[serde(default)]
    pub realtime: bool,
}

/// Configuration for a single task
#[derive(Debug, Deserialize, Serialize)]
pub struct HostTask {
    /// Task name.
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
}

static CONFIG: OnceLock<HostConfig> = OnceLock::new();

pub(crate) fn config() -> &'static HostConfig {
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
        // TODO: Validation? Only one idle task? All executables exist?
    })
}
