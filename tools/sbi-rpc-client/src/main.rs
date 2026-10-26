// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client for the `sbi-rpc` task, which exposes SB-RMI and SB-TSI commands
//! over UDP.

use std::{fmt::Debug, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use nprpc::{
    ClientError, ClientIoError, Response, autobuffer, compose_interfaces,
    wire::WireError,
};
use nprpc_udp::UdpClient;
use serde::Deserialize;

use task_sbi_rpc_api::{
    SbRmiCallWriteReadRequest, SbTsiWriteRequest, sp3_raw_sb_rmi::Client as _,
    sp3_raw_sb_tsi::Client as _, sp3_sb_rmi::Client as _,
    sp3_sb_tsi::Client as _,
};

// Every interface the server might serve, so our buffers are big enough for any
// request/response. The raw interfaces are only served by `-dev` images.
compose_interfaces! {
    mod: all_rpcs,
    interfaces: [
        task_sbi_rpc_api::sp3_raw_sb_rmi,
        task_sbi_rpc_api::sp3_raw_sb_tsi,
        task_sbi_rpc_api::sp3_sb_tsi,
        task_sbi_rpc_api::sp3_sb_rmi,
    ]
}
autobuffer!(ApiBufs, all_rpcs);

type MyClient = UdpClient<ApiBufs>;

/// Issue SB-RMI and SB-TSI commands to a Hubris `sbi-rpc` server over UDP.
///
/// Connection options go before the command, e.g.
/// `sbi-rpc-client --target .. read-cpu-temp`.
#[derive(Parser)]
struct Args {
    /// Remote address of the RPC server, e.g. `[fe80::c1d:7dff:feef:9f1d%2]:5501`
    #[clap(long)]
    target: SocketAddr,

    /// How long to wait for each response, in milliseconds
    #[clap(long, default_value = "1000")]
    timeout_ms: u64,

    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Issue a sequence of commands from a TOML file
    Batch {
        /// The path to a TOML file containing an array of `[[op]]` tables.
        ///
        /// Each op table represents a command to send to the SP. Commands
        /// are sent in the order they are defined in the TOML file.
        ///
        /// Op tables consist of a `cmd` key, which indicates the command
        /// to send (in `snake_case`), followed by keys for that command's
        /// arguments. For example:
        ///
        /// `{ cmd = "read_cpu_temp" }`
        /// `{ cmd = "smm_write_read_word", command = N, arg = N }`
        #[clap(long)]
        ops: PathBuf,

        /// Keep going after a failed op instead of halting
        #[clap(long)]
        keep_going: bool,
    },

    #[clap(flatten)]
    Op(Op),
}

/// Top level of a batch ops file
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpsFile {
    op: Vec<Op>,
}

/// A single command, either from the command line or a batch file.
///
/// Commands without arguments are empty struct variants rather than unit
/// variants, so that `deny_unknown_fields` still applies to them.
#[derive(Subcommand, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum Op {
    // SB-TSI
    /// Read the CPU temperature, in degrees C
    ReadCpuTemp {},
    /// Read the SB-TSI status register
    ReadStatus {},
    /// Read the SB-TSI config register
    ReadConfig {},
    /// Read the SB-TSI temperature update rate register
    ReadUpdateRate {},
    /// Read the high temperature threshold, in degrees C
    ReadHighTempThreshold {},
    /// Read the low temperature threshold, in degrees C
    ReadLowTempThreshold {},
    /// Read the SB-TSI timeout config register
    ReadTimeoutConfig {},
    /// Read the SB-TSI alert threshold register
    ReadAlertThreshold {},
    /// Read the SB-TSI alert config register
    ReadAlertConfig {},
    /// Read the SB-TSI manufacture ID
    ReadManufactureId {},
    /// Read the SB-TSI revision
    ReadRevision {},

    // SB-RMI mailbox
    /// Read the package power consumption
    ReadPackagePowerConsumption {},
    /// Read the package power limit
    ReadPackagePowerLimit {},
    /// Read the maximum package power limit
    ReadMaxPackagePowerLimit {},
    /// Read the thermal design power
    ReadTdp {},
    /// Read the maximum configurable TDP
    ReadMaxCtdp {},
    /// Read the minimum configurable TDP
    ReadMinCtdp {},
    /// Read the BIOS boost Fmax
    ReadBiosBoostFmax {
        /// Command argument, see the PPR for its meaning
        #[clap(long, value_parser = parse_int::<u16>)]
        arg: u16,
    },
    /// Read the APML boost limit
    ReadApmlBoostLimit {
        /// Command argument, see the PPR for its meaning
        #[clap(long, value_parser = parse_int::<u16>)]
        arg: u16,
    },
    /// Read the DRAM throttle
    ReadDramThrottle {},
    /// Read the PROCHOT status
    ReadProchotStatus {},
    /// Read the PROCHOT residency
    ReadProchotResidency {},
    /// Read the VDDIO memory power
    ReadVddioMemPower {},
    /// Read NBIO error logging registers
    ReadNbioErrorLogging {
        /// Command argument, see the PPR for its meaning
        #[clap(long, value_parser = parse_int::<u32>)]
        arg: u32,
    },
    /// Read the IOD BIST result
    ReadIodBistResult {},
    /// Read a CCD BIST result
    ReadCcdBistResult {
        /// Command argument, see the PPR for its meaning
        #[clap(long, value_parser = parse_int::<u32>)]
        arg: u32,
    },
    /// Read a CCX BIST result
    ReadCcxBistResult {
        /// Command argument, see the PPR for its meaning
        #[clap(long, value_parser = parse_int::<u32>)]
        arg: u32,
    },
    /// Read the maximum DDR bandwidth utilization
    ReadMaxDdrBwUtil {},

    // Raw commands, only served by `-dev` images
    /// (dev only) Issue an SB-RMI mailbox command with no argument
    SmmReadWord {
        /// Mailbox command byte
        #[clap(long, value_parser = parse_int::<u8>)]
        command: u8,
    },
    /// (dev only) Issue an SB-RMI mailbox command with a word argument
    SmmWriteReadWord {
        /// Mailbox command byte
        #[clap(long, value_parser = parse_int::<u8>)]
        command: u8,

        /// Command argument
        #[clap(long, value_parser = parse_int::<u32>)]
        arg: u32,
    },
    /// (dev only) Read a single byte from an SB-TSI register
    TsiReadByte {
        /// SB-TSI register
        #[clap(long, value_parser = parse_int::<u8>)]
        reg: u8,
    },
    /// (dev only) Write a single byte to an SB-TSI register
    TsiWriteByte {
        /// SB-TSI register
        #[clap(long, value_parser = parse_int::<u8>)]
        reg: u8,

        /// Byte to write
        #[clap(long, value_parser = parse_int::<u8>)]
        val: u8,
    },

    /// Pause for `ms` milliseconds, mostly useful in batches
    Sleep {
        #[clap(long)]
        ms: u64,
    },
}

/// Parses an integer in decimal, or hex with a `0x` prefix.
fn parse_int<T>(s: &str) -> Result<T>
where
    T: TryFrom<u32>,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    let v = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16)?,
        None => s.parse()?,
    };
    Ok(v.try_into()?)
}

fn main() -> Result<()> {
    let args = Args::parse();

    let (ops, keep_going) = match args.cmd {
        Cmd::Batch { ops, keep_going } => {
            let ops_toml = std::fs::read_to_string(&ops)
                .with_context(|| format!("reading {}", ops.display()))?;
            let ops: OpsFile = toml::from_str(&ops_toml)
                .with_context(|| format!("parsing {}", ops.display()))?;
            (ops.op, keep_going)
        }
        Cmd::Op(op) => {
            let mut client = connect(args.target, args.timeout_ms)?;
            return run_op(&mut client, &op);
        }
    };

    let mut client = connect(args.target, args.timeout_ms)?;
    let mut failures = 0;
    for (i, op) in ops.iter().enumerate() {
        if let Err(e) = run_op(&mut client, op) {
            eprintln!("op {i}: {e}");
            failures += 1;
            if !keep_going {
                bail!("halting after failed op {i}");
            }
        }
    }

    if failures > 0 {
        bail!("{failures} of {} ops failed", ops.len());
    }

    Ok(())
}

/// Connects to the server at `target`.
fn connect(target: SocketAddr, timeout_ms: u64) -> Result<MyClient> {
    let client = UdpClient::connect(target, ApiBufs::new())
        .with_context(|| format!("connecting to {target}"))?;
    client
        .set_timeout(Some(Duration::from_millis(timeout_ms)))
        .context("setting read timeout")?;
    Ok(client)
}

/// Issues one op, printing its result on success.
fn run_op(client: &mut MyClient, op: &Op) -> Result<()> {
    match *op {
        Op::ReadCpuTemp {} => show(client.read_cpu_temp(&())),
        Op::ReadStatus {} => show(client.read_status(&())),
        Op::ReadConfig {} => show(client.read_config(&())),
        Op::ReadUpdateRate {} => show(client.read_update_rate(&())),
        Op::ReadHighTempThreshold {} => {
            show(client.read_high_temp_threshold(&()))
        }
        Op::ReadLowTempThreshold {} => {
            show(client.read_low_temp_threshold(&()))
        }
        Op::ReadTimeoutConfig {} => show(client.read_timeout_config(&())),
        Op::ReadAlertThreshold {} => show(client.read_alert_threshold(&())),
        Op::ReadAlertConfig {} => show(client.read_alert_config(&())),
        Op::ReadManufactureId {} => show(client.read_manufacture_id(&())),
        Op::ReadRevision {} => show(client.read_revision(&())),

        Op::ReadPackagePowerConsumption {} => {
            show(client.read_package_power_consumption(&()))
        }
        Op::ReadPackagePowerLimit {} => {
            show(client.read_package_power_limit(&()))
        }
        Op::ReadMaxPackagePowerLimit {} => {
            show(client.read_max_package_power_limit(&()))
        }
        Op::ReadTdp {} => show(client.read_tdp(&())),
        Op::ReadMaxCtdp {} => show(client.read_max_ctdp(&())),
        Op::ReadMinCtdp {} => show(client.read_min_ctdp(&())),
        Op::ReadBiosBoostFmax { arg } => {
            show(client.read_bios_boost_fmax(&arg))
        }
        Op::ReadApmlBoostLimit { arg } => {
            show(client.read_apml_boost_limit(&arg))
        }
        Op::ReadDramThrottle {} => show(client.read_dram_throttle(&())),
        Op::ReadProchotStatus {} => show(client.read_prochot_status(&())),
        Op::ReadProchotResidency {} => show(client.read_prochot_residency(&())),
        Op::ReadVddioMemPower {} => show(client.read_vddio_mem_power(&())),
        Op::ReadNbioErrorLogging { arg } => {
            show(client.read_nbio_error_logging(&arg))
        }
        Op::ReadIodBistResult {} => show(client.read_iod_bist_result(&())),
        Op::ReadCcdBistResult { arg } => {
            show(client.read_ccd_bist_result(&arg))
        }
        Op::ReadCcxBistResult { arg } => {
            show(client.read_ccx_bist_result(&arg))
        }
        Op::ReadMaxDdrBwUtil {} => show(client.read_max_ddr_bw_util(&())),

        Op::SmmReadWord { command } => show(client.smm_read_word(&command)),
        Op::SmmWriteReadWord { command, arg } => {
            show(client.smm_write_read_word(&SbRmiCallWriteReadRequest {
                command,
                arg,
            }))
        }
        Op::TsiReadByte { reg } => show(client.read_byte(&reg)),
        Op::TsiWriteByte { reg, val } => {
            show(client.write_byte(&SbTsiWriteRequest { reg, val }))
        }

        Op::Sleep { ms } => {
            std::thread::sleep(Duration::from_millis(ms));
            Ok(())
        }
    }
}

/// Prints the result of an RPC, or turns it into an error.
fn show<T: Value, E: Debug>(
    resp: Result<Response<Result<T, E>>, ClientIoError<std::io::Error>>,
) -> Result<()> {
    let body = resp.map_err(|e| match e {
        ClientIoError::Client(ClientError::Server(
            WireError::SERVER_UNKNOWN_METHOD,
        )) => anyhow!(
            "server doesn't support this command \
             (raw commands are only served by -dev images)"
        ),
        e => anyhow!("RPC failed: {e:?}"),
    })?;
    let val = body.body.map_err(|e| anyhow!("server error: {e:?}"))?;
    println!("ok{}", val.display());
    Ok(())
}

/// Formatting for values returned by the server
trait Value {
    /// Formats the value to follow "ok", e.g. ", read 0x12 (18)"
    fn display(&self) -> String;
}

impl Value for () {
    fn display(&self) -> String {
        String::new()
    }
}

impl Value for u8 {
    fn display(&self) -> String {
        format!(", read {self:#04x} ({self})")
    }
}

impl Value for u32 {
    fn display(&self) -> String {
        format!(", read {self:#010x} ({self})")
    }
}

impl Value for f32 {
    fn display(&self) -> String {
        format!(", read {self}")
    }
}
