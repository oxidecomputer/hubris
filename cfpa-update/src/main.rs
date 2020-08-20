use nxp_structs::*;
use openssl::sha;
use packed_struct::prelude::*;
use serde_json::*;
use std::io::{Read, Write};
use std::process::Command;
use structopt::StructOpt;
use tempfile::NamedTempFile;
use std::path::PathBuf;

#[derive(StructOpt)]
#[structopt(name = "cfpa_setup", max_term_width = 80)]
struct Args {
    #[structopt(parse(from_os_str), long, short, value_name = "blhost_path")]
    blhost: PathBuf,
    #[structopt(parse(from_os_str), long, short, value_name = "isp_port")]
    isp_port: PathBuf,
    #[structopt(long, short, value_name = "outfile")]
    outfile: PathBuf,
}

fn main() {
    let args = Args::from_args();

    let ping = Command::new(&args.blhost)
        .arg("-p")
        .arg(&args.isp_port)
        .arg("-j")
        .arg("--")
        .arg("get-property")
        .arg("1")
        .output();

    let result: Value = serde_json::from_slice(&ping.unwrap().stdout).unwrap();

    if result["status"]["value"] != 0 {
        println!("blhost ping failed {:?}", result["status"]["description"]);
        println!("make sure you are in ISP mode");
        return;
    }

    let cfpa = NamedTempFile::new().unwrap();

    // 0x9de00 is the fixed address of the CFPA region
    let get = Command::new(&args.blhost)
        .arg("-p")
        .arg(&args.isp_port)
        .arg("-j")
        .arg("--")
        .arg("read-memory")
        .arg("0x9de00")
        .arg("512")
        .arg(cfpa.path())
        .output();

    let result: Value = serde_json::from_slice(&get.unwrap().stdout).unwrap();

    if result["status"]["value"] != 0 {
        println!(
            "reading CFPA memory failed {:?}",
            result["status"]["description"]
        );
        return;
    }

    // Need to read the CFPA back in to modify
    let mut cfpa_array: [u8; 512] = [0; 512];
    let mut target = cfpa.reopen().unwrap();
    target.read_exact(&mut cfpa_array).unwrap();

    let mut cfpa: CFPAPage = CFPAPage::unpack(&cfpa_array).unwrap();

    // We always need to bump the version
    cfpa.version = cfpa.version + 1;

    // Ensure the first certificate is valid
    cfpa.rotkh_revoke.rotk0 = 0x1.into();

    let mut updated = cfpa.pack();

    // need to recalculate sha over the updated data
    let mut sha = sha::Sha256::new();
    sha.update(&updated[..0x1e0]);

    let updated_sha = sha.finish();

    updated[0x1e0..].clone_from_slice(&updated_sha);

    let mut new_cfpa = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(&args.outfile)
        .unwrap();

    new_cfpa.write_all(&updated).unwrap();

    println!("done! new CFPA file written to {}", &args.outfile.to_string_lossy());
}
