use std::error::Error;
use std::path::PathBuf;

use structopt::StructOpt;

mod packager;

#[derive(Debug, StructOpt)]
#[structopt(
    max_term_width = 80,
    about = "extra tasks to help you work on Hubris"
)]
enum Xtask {
    /// Builds a collection of cross-compiled binaries at non-overlapping addresses,
    /// and then combines them into a system image with an application descriptor.
    Packager {
        /// Path to the image configuration file, in TOML.
        cfg: PathBuf,

        /// Path to the output directory, where this tool will place a set of ELF
        /// files, a combined SREC file, and a text file documenting the memory
        /// layout.
        out: PathBuf,
    },
}

fn main() -> Result<(), Box<dyn Error>> {
    let xtask = Xtask::from_args();
    println!("{:?}", xtask);
    match xtask {
        Xtask::Packager { cfg, out } => {
            packager::package(cfg, out)?;
        }
    }

    Ok(())
}
