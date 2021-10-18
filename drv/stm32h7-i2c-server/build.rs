use std::env;
use std::fs::File;
use std::path::Path;
use anyhow::Result;

use build_i2c::{I2cConfigGenerator, I2cConfigDisposition};

fn codegen() -> Result<()> {
    use std::io::Write;

    let out_dir = env::var("OUT_DIR")?;
    let dest_path = Path::new(&out_dir).join("config.rs");
    let mut file = File::create(&dest_path)?;

    let mut g = I2cConfigGenerator::new(I2cConfigDisposition::Initiator);

    g.generate_header()?;
    g.generate_controllers()?;
    g.generate_pins()?;
    g.generate_muxes()?;
    g.generate_footer()?;

    file.write_all(g.output.as_bytes())?;

    Ok(())
}

fn main() {
    build_util::expose_target_board();

    if let Err(e) = codegen() {
        println!("code generation failed: {}", e);
        std::process::exit(1);
    }

    println!("cargo:rerun-if-changed=build.rs");
}
