use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[clap(max_term_width = 80, about = "FPGA codegen")]
struct Args {
    /// Register description (JSON)
    #[clap(short, long)]
    source: PathBuf,
    #[clap(short, long, parse(try_from_str = parse_int::parse), default_value_t=0x60000000)]
    base_addr: u32,
    /// Top-level register map (JSON), which provides global peripheral offsets
    #[clap(short, long)]
    top: Option<PathBuf>,
    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Build an address map
    AddrMap,
    /// Build a register map
    RegMap,
    /// Build a peripheral
    Peripheral,
}

pub fn main() -> Result<()> {
    let args = Args::parse();
    let mut output = String::new();
    let node = build_fpga_regmap::read_parse(&args.source)?;
    match args.cmd {
        Cmd::AddrMap => build_fpga_regmap::build_addr_map(&node, &mut output),
        Cmd::RegMap => build_fpga_regmap::build_reg_map(&node, &mut output),
        Cmd::Peripheral => {
            let Some(top) = args.top else {
                bail!("must provide --top register map");
            };
            let source = args.source.file_stem().unwrap().to_str().unwrap();
            let Some(peripheral) = source.strip_suffix("_reg_map") else {
                bail!(
                    "could not get peripheral name from {source},
                     expected '_reg_map' suffix"
                );
            };
            let top = build_fpga_regmap::read_parse(&top)?;
            output = build_fpga_regmap::build_peripheral(
                &node,
                &top,
                peripheral,
                args.base_addr,
                None,
            )?;
        }
    }
    println!("{output}");

    Ok(())
}
