// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel Configuration
//!
//! CLI and runtime configuration settings for the kernel's execution.

use clap::Parser;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Task descriptor for host-run tasks.
pub(crate) struct HostTaskDesc {
    /// The name of the task
    pub name: &'static str,
    /// Whether this is the "idle" task (which will not be spawned or run),
    pub idle: bool,
}

include!(concat!(env!("OUT_DIR"), "/host_tasks.rs"));

/// Host-run kernel CLI args
#[derive(Parser)]
#[clap(about = "Host-Run Kernel")]
struct Args {
    /// Stop after this many milliseconds of execution
    #[clap(long, value_name = "TICKS")]
    stop_at: Option<u64>,
    /// Setting 'realtime' causes the kernel to pause in the idle task for the
    /// required amount of time, instead of "fast forwarding" time.
    #[clap(long)]
    realtime: bool,
    /// Output system call trace information for ALL tasks.
    #[clap(long, conflicts_with = "trace-only")]
    trace: bool,
    /// Output system call trace information for the given list of tasks.
    #[clap(long, value_name = "TASKS", value_delimiter = ',')]
    trace_only: Vec<String>,
    /// Path to folder containing all of the separately built task executables,
    /// which will be spawned by the kernel.
    #[clap(long, value_name = "DIR")]
    task_dir: Option<PathBuf>,
}

/// Trace Filter Options.
pub(crate) enum TraceFilter {
    /// Don't emit any traces.
    Off,
    /// Emit traces for all tasks.
    All,
    /// Only syscalls involving one of these task indices.
    Tasks(HashSet<usize>),
}

/// Post-processed version of [`Args`], stored after parsing CLI args once.
pub(crate) struct HostOptions {
    /// Stop after this many milliseconds of execution
    pub stop_at: Option<u64>,
    /// Setting 'realtime' causes the kernel to pause in the idle task for the
    /// required amount of time, instead of "fast forwarding" time.
    pub realtime: bool,
    /// Tracing settings.
    pub trace: TraceFilter,
    /// Path to folder containing all of the separately built task executables,
    /// which will be spawned by the kernel.
    pub task_dir: PathBuf,
}

static OPTIONS: OnceLock<HostOptions> = OnceLock::new();

pub(crate) fn options() -> &'static HostOptions {
    OPTIONS.get_or_init(|| {
        let args = Args::try_parse().unwrap_or_else(|e| e.exit());

        let trace = if args.trace {
            TraceFilter::All
        } else if args.trace_only.is_empty() {
            TraceFilter::Off
        } else {
            let indices = args
                .trace_only
                .iter()
                .map(|name| {
                    let name = name.trim();
                    HOST_TASKS
                        .iter()
                        .position(|t| t.name == name)
                        .unwrap_or_else(|| {
                            fatal!("--trace-only names unknown task {name:?}")
                        })
                })
                .collect();
            TraceFilter::Tasks(indices)
        };

        let task_dir = args.task_dir.unwrap_or_else(|| {
            let exe = std::env::current_exe().unwrap_or_else(|e| {
                fatal!("finding the kernel executable: {e}")
            });
            match exe.parent() {
                Some(dir) => dir.to_path_buf(),
                None => fatal!(
                    "kernel executable {} has no parent directory",
                    exe.display()
                ),
            }
        });

        HostOptions {
            stop_at: args.stop_at,
            realtime: args.realtime,
            trace,
            task_dir,
        }
    })
}
