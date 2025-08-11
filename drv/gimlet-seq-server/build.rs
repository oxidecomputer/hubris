// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use build_fpga_regmap::fpga_regs;
use serde::Deserialize;
use sha2::Digest;
use std::{fs, io::Write, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    fpga_image: String,
    register_defs: String,
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    build_util::expose_target_board();
    build_util::build_notifications()?;
    build_stm32xx_sys::build_gpio_irq_pins()?;

    let config = build_util::task_config::<Config>()?;

    let fpga_image_path = PathBuf::from(&config.fpga_image);

    if fpga_image_path.components().count() != 1 {
        panic!("fpga_image path mustn't contain a slash, sorry.");
    }

    let fpga_image = fs::read(&fpga_image_path)?;
    let compressed = gnarle::compress_to_vec(&fpga_image);

    let out = build_util::out_dir();
    let compressed_path = out.join(fpga_image_path.with_extension("bin.rle"));
    fs::write(&compressed_path, &compressed)?;
    println!("cargo::rerun-if-changed={}", config.fpga_image);

    println!(
        "cargo::rustc-env=GIMLET_FPGA_IMAGE_PATH={}",
        compressed_path.display()
    );

    let disposition = build_i2c::Disposition::Devices;
    if let Err(e) = build_i2c::codegen(build_i2c::CodegenSettings {
        disposition,
        include_refdes: true,
    }) {
        println!("cargo::error=code generation failed: {e}");
        std::process::exit(1);
    }

    let regs_in = PathBuf::from(config.register_defs);
    println!("cargo:rerun-if-changed={}", regs_in.display());
    let regs_in_txt = fs::read_to_string(&regs_in)?;

    let regs_out = out.join(regs_in.with_extension("rs"));
    println!("cargo:rustc-env=GIMLET_FPGA_REGS={}", regs_out.display());

    // Write the FPGA register map
    let mut file = fs::File::create(regs_out)?;
    write!(&mut file, "{}", fpga_regs(&regs_in_txt)?)?;

    // Calculate a bitstream checksum and add it to the generated Rust file
    let mut hasher = sha2::Sha512::new();
    hasher.update(&compressed);
    let result = hasher.finalize();
    writeln!(
        &mut file,
        "\npub const GIMLET_BITSTREAM_CHECKSUM: u32 = {:#x};",
        u32::from_le_bytes(result[..4].try_into().unwrap())
    )?;

    idol::Generator::new().build_server_support(
        "../../idl/cpu-seq.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    Ok(())
}
