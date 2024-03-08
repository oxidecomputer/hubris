// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
use build_lpc55pins::PinConfig;
use serde::Deserialize;
use std::io::Write;

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

    let task_config = build_util::task_config::<TaskConfig>()?;

    generate_swd_functions(&task_config)?;
    build_lpc55pins::codegen(task_config.pins)?;

    Ok(())
}
