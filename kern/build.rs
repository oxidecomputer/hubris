use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_m_profile();

    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut const_file = File::create(out.join("consts.rs")).unwrap();

    writeln!(const_file, "pub const EXC_RETURN_CONST : u32 = 0xFFFFFFED;")
        .unwrap();
    Ok(())
}
