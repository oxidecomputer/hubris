fn main() {
    build_util::expose_target_board();

    #[cfg(feature = "standalone")]
    let disposition = build_i2c::I2cConfigDisposition::Standalone;

    #[cfg(not(feature = "standalone"))]
    let disposition = build_i2c::I2cConfigDisposition::DevicesOnly;

    if let Err(e) = build_i2c::codegen(disposition) {
        println!("code generation failed: {}", e);
        std::process::exit(1);
    }

    println!("cargo:rerun-if-changed=build.rs");
}
