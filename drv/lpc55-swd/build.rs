// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::{bail, Context, Result};
use build_lpc55pins::PinConfig;
use call_rustfmt::rustfmt;
use goblin::container::Container;
use goblin::elf::section_header::{SectionHeader, SHF_ALLOC, SHT_PROGBITS};
use goblin::elf::Elf;
use serde::Deserialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

// Symbols relied on in the endoscope.elf file.
// The image load address.
pub const LOAD_SYMBOL: &str = "__vector_table";
// An instance of struct Shared is expected at this address
pub const SHARED_STRUCT_SYMBOL: &str = "SHARED";
// The reset vector found in the image should match this symbol value.
pub const RESET_VECTOR_SYMBOL: &str = "Reset";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskConfig {
    in_cfg: Vec<PinConfig>,
    out_cfg: Vec<PinConfig>,
    pins: Vec<PinConfig>,
    spi_num: usize,
}

fn generate_swd_functions(config: &TaskConfig) -> Result<()> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("swd.rs");
    let mut file = std::fs::File::create(dest_path)?;

    let out_cfg = &config.out_cfg;
    let in_cfg = &config.in_cfg;
    let spi_periph = quote::format_ident!("Fc{}", config.spi_num);
    let flexcomm = quote::format_ident!("FLEXCOMM{}", config.spi_num);
    let spi_regs = quote::format_ident!("SPI{}", config.spi_num);

    // The RoT -> SP SWD control requires setting the IO functions at runtime
    // as opposed to just startup.
    //
    // It turns out to be very expensive to call into the GPIO task for
    // changing pin direction so these functions access the IOCON block
    // directly.
    writeln!(
        &mut file,
        "{}",
        quote::quote! {

        // SAFETY: we're relying on the enum value of the pin for correctness
        // here. The LPC55 IOCON Rust API has individual functions for each
        // pin which we aren't easy to use with a task based setup but
        // could actually be used here. It's not clear how much benefit
        // we'd actually get from that though.

        // io_out = MOSI on, MISO off
        fn switch_io_out() {
            let iocon_base = lpc55_pac::IOCON::ptr() as *const u32 as u32;

            #(
            {
                use drv_lpc55_gpio_api::*;

                let (pin, conf) = drv_lpc55_gpio_api::Pins::iocon_conf_val(#out_cfg);
                let base = iocon_base + 4 * pin;
                unsafe {
                    core::ptr::write_volatile(base as *mut u32, conf);
                }
            })*
        }
        // io_in = MOSI off, MISO on
        fn switch_io_in() {
            let iocon_base = lpc55_pac::IOCON::ptr() as *const u32 as u32;

            #(
            {
                use drv_lpc55_gpio_api::*;
                let (pin, conf) = drv_lpc55_gpio_api::Pins::iocon_conf_val(#in_cfg);
                let base = iocon_base + 4 * pin;
                unsafe {
                    core::ptr::write_volatile(base as *mut u32, conf);
                }
            })*
        }
        fn setup_spi(task : TaskId) -> spi_core::Spi {
            let syscon = Syscon::from(task);
            syscon.enable_clock(Peripheral::#spi_periph);
            syscon.leave_reset(Peripheral::#spi_periph);
            let flexcomm = unsafe { &*device::#flexcomm::ptr() };
            flexcomm.pselid.write(|w| w.persel().spi());
            let registers = unsafe { &*device::#spi_regs::ptr() };
            spi_core::Spi::from(registers)
        }
        }
    )?;

    Ok(())
}

fn prepare_endoscope() -> Result<(), anyhow::Error> {
    let key = "CARGO_BIN_FILE_ENDOSCOPE";
    println!("cargo:rerun-if-env-changed={key}");
    let elf_path = PathBuf::from(
        std::env::var(key)
            .with_context(|| format!("Cannot read env var '{}'", key))?,
    );
    let data = std::fs::read(&elf_path).context("could not open ELF file")?;
    let elf = Elf::parse(&data).context("cannot parse ELF file")?;

    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("endoscope.rs");
    let bin_path = out_dir.join("endoscope.bin");
    let mut file = std::fs::File::create(&dest_path)
        .context("Cannot create output file")?;

    writeln!(&mut file, "mod endoscope {{")?;

    // Get entry point
    if elf.header.container()? != Container::Little
        || elf.header.e_machine != goblin::elf::header::EM_ARM
    {
        bail!("Not a little endian ARM ELF file");
    }

    let interesting = std::collections::BTreeMap::from([
        // Load address of image
        (LOAD_SYMBOL, "LOAD"),
        // Address of endoscope output struct
        (SHARED_STRUCT_SYMBOL, "SHARED"),
    ]);

    for sym in elf.syms.iter() {
        if let Some(name) = elf.strtab.get_at(sym.st_name) {
            // Note: If we were using Rust instead of linker symbols
            // we would want to use rustc_demangle::demangle(str)
            if let Some(myname) = interesting.get(name) {
                writeln!(
                    &mut file,
                    "pub const {}: u32 = {:#x};",
                    myname, sym.st_value
                )?;
            }
        }
    }
    writeln!(&mut file, "}}")?;

    // Extract image bits from the ELF file.
    let elf_reader = OpenOptions::new().read(true).open(&elf_path).unwrap();
    let bin = get_elf(elf_reader).with_context(|| {
        format!("cannot extract bin from elf {}", elf_path.display())
    })?;
    std::fs::write(&bin_path, bin).context("cannot write to {&bin_path}")?;

    writeln!(
        &mut file,
        "// Bytes extracted from target/{}",
        elf_path.to_str().unwrap().split("target/").last().unwrap()
    )?;
    writeln!(
        &mut file,
        "const ENDOSCOPE_BYTES: &[u8] = include_bytes!(r#\"{}\"#);",
        bin_path.to_str().unwrap()
    )?;
    drop(file);
    rustfmt(&dest_path).context("cannot call_rustfmt")?;

    Ok(())
}

/// Given a reader on an ELF file return the executable image.
pub fn get_elf<R>(mut reader: R) -> Result<Vec<u8>, anyhow::Error>
where
    R: Read + Seek,
{
    let mut data = vec![];
    let _len = reader
        .read_to_end(&mut data)
        .context("cannot read to end")?;
    let elf = Elf::parse(&data).context("cannot parse data as ELF")?;

    if elf.header.container()? != Container::Little
        || elf.header.e_machine != goblin::elf::header::EM_ARM
    {
        bail!("not a little-endian ARM ELF file");
    }

    // Extract the bytes to load

    // Find all of the sections that compose our '.bin' file.
    // These criteria were inferred from the produced artifact
    // and the result of `arm-none-eabi-objcopy -O binary $INPUT_ELF $OUTPUT_BIN`
    //
    // See also:
    // https://refspecs.linuxfoundation.org/LSB_2.1.0/LSB-Embedded/LSB-Embedded/elftypes.html
    let sections: Vec<&SectionHeader> = elf
        .section_headers
        .iter()
        .filter(|sh| (sh.sh_type & SHT_PROGBITS == SHT_PROGBITS))
        .filter(|sh| (sh.sh_flags as u32 & SHF_ALLOC == SHF_ALLOC))
        .filter(|sh| !sh.vm_range().is_empty())
        .collect();
    let bin_size: usize = sections.iter().map(|sh| sh.vm_range().len()).sum();

    // Do a sanity check on the size of the blob based on what we've seen.
    //
    // Notes:
    //
    // Clippy ignores the profile (or equivalent) when building the
    // endoscope blob which results in an enormous binary. Since we're not
    // going to actually build the swd task when running clippy, the large
    // size can be ignored.
    //
    // The artifact-specific profiles do not allow `lto` to be
    // specified. That would save at least 500 bytes of executable.
    // In Cargo.toml, see [profile.*.package.endoscope]
    //
    // TODO: Add to or create relevant cargo bug(s):
    // e.g. https://github.com/rust-lang/cargo/issues/11680
    //
    #[cfg(not(clippy))]
    if bin_size > 6 * 1024 {
        bail!("bin_size of {bin_size} is over 6KiB. Was it built with the wrong profile?");
    }

    // Test our assumptions that the sections are in order and contiguous.
    if sections.len() > 1 {
        for index in 0..(sections.len() - 1) {
            if sections[index].vm_range().end
                != sections[index + 1].vm_range().start
            {
                bail!("discontiguous sections {} and {}", index, index + 1);
            }
        }
    }

    // We're also assuming that `vm_range`s go from 0 to bin_size.
    // So, expect a panic if we now try to read outside those limits.
    let mut bin = vec![0u8; bin_size];

    for section in sections {
        let file_range = section.file_range().unwrap();
        let vm_range = section.vm_range();
        reader.seek(SeekFrom::Start(file_range.start as u64))?;
        reader
            .read_exact(&mut bin[vm_range.start..vm_range.end])
            .with_context(|| {
                format!(
                    "cannot read elf[{}..] into bin[{}..{}]",
                    file_range.start, vm_range.start, vm_range.end
                )
            })?;
    }
    Ok(bin)
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    idol::Generator::new()
        .with_counters(
            idol::CounterSettings::default().with_server_counters(false),
        )
        .build_server_support(
            "../../idl/sp-ctrl.idol",
            "server_stub.rs",
            idol::server::ServerStyle::InOrder,
        )?;

    build_util::expose_target_board();
    build_util::build_notifications()?;

    let task_config = build_util::task_config::<TaskConfig>()?;

    generate_swd_functions(&task_config)?;
    build_lpc55pins::codegen(task_config.pins)?;

    prepare_endoscope()?;

    Ok(())
}
