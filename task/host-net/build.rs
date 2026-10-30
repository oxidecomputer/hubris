// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Write;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

/// `[tasks.net.config]` for the host net task.
#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct HostNetConfig {
    /// Added to every socket's port when binding, so that a manifest's real
    /// port numbers (many below 1024) can be used without privileges.
    #[serde(default)]
    port_offset: u16,
    /// Host address to bind every socket to; `::` (all interfaces, IPv4 and
    /// IPv6) by default.
    bind_address: Option<String>,
    /// How often, in ticks, to check the host sockets for traffic.
    poll_interval: Option<u32>,
}

fn main() -> Result<()> {
    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/net.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )
        .map_err(|e| anyhow!("{e}"))?;
    build_util::build_notifications()?;

    let net = build_net::load_net_config()?;
    let cfg =
        build_util::task_maybe_config::<HostNetConfig>()?.unwrap_or_default();
    let tasks = build_util::task_ids();

    let mut names = Vec::new();
    let mut ports = Vec::new();
    let mut owners = Vec::new();
    let mut untrusted = Vec::new();
    // Same order as `SocketName` in task-net-api: the sockets map is sorted.
    for (name, socket) in &net.sockets {
        if socket.kind != "udp" {
            bail!("socket {name}: unsupported kind {:?}", socket.kind);
        }
        let port =
            socket.port.checked_add(cfg.port_offset).with_context(|| {
                format!("socket {name}: port-offset pushes port past 65535")
            })?;
        let owner = &socket.owner.name;
        let index = tasks.get(owner).with_context(|| {
            format!("socket {name} is owned by {owner}, which is not a task")
        })?;
        let mask = format!(
            "crate::notifications::{owner}::{}_MASK",
            socket.owner.notification.to_uppercase().replace('-', "_")
        );
        names.push(format!("{name:?}"));
        ports.push(port.to_string());
        owners.push(format!("({index}, {mask})"));
        untrusted.push(socket.allow_untrusted.to_string());
    }

    let n = names.len();
    let bind = cfg.bind_address.as_deref().unwrap_or("::");
    bind.parse::<std::net::IpAddr>().with_context(|| {
        format!("bind-address {bind:?} is not an IP address")
    })?;
    let poll = cfg.poll_interval.unwrap_or(5);
    if poll == 0 {
        bail!("poll-interval must be at least 1 tick");
    }

    let mut out = std::fs::File::create(
        build_util::out_dir().join("host_net_config.rs"),
    )?;
    writeln!(out, "pub const SOCKET_COUNT: usize = {n};")?;
    writeln!(
        out,
        "pub const SOCKET_NAMES: [&str; {n}] = [{}];",
        names.join(", ")
    )?;
    writeln!(
        out,
        "pub const SOCKET_PORTS: [u16; {n}] = [{}];",
        ports.join(", ")
    )?;
    writeln!(
        out,
        "/// Owner task index and the notification mask to post it.\n\
         pub const SOCKET_OWNERS: [(u16, u32); {n}] = [{}];",
        owners.join(", ")
    )?;
    writeln!(
        out,
        "#[allow(dead_code)]\n\
         pub const SOCKET_ALLOW_UNTRUSTED: [bool; {n}] = [{}];",
        untrusted.join(", ")
    )?;
    writeln!(out, "pub const BIND_ADDRESS: &str = {bind:?};")?;
    writeln!(out, "pub const POLL_INTERVAL: u32 = {poll};")?;
    Ok(())
}
