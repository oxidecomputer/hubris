// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use indexmap::IndexMap;
use proc_macro2::TokenStream;
use quote::{ToTokens, TokenStreamExt};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_target_board();

    let task_config = build_util::task_config::<TaskConfig>()?;
    let global_config = build_util::config::<GlobalConfig>()?;
    check_spi_config(&global_config.spi, &task_config.spi)?;
    generate_spi_config(&global_config.spi, &task_config.spi)?;

    idol::server::build_server_support(
        "../../idl/spi.idol",
        "server_stub.rs",
        idol::server::ServerStyle::InOrder,
    )?;

    Ok(())
}

///////////////////////////////////////////////////////////////////////////////
// SPI config schema definition.
//
// There are two portions to this, task-level and global. Both are defined by
// the structs below using serde.
//
// Task-level simply provides a way (through `global_config`) to reference a key
// in the global.
//
// Global starts at `GlobalConfig`.

#[derive(Deserialize)]
struct TaskConfig {
    spi: SpiTaskConfig,
}

#[derive(Deserialize)]
struct SpiTaskConfig {
    global_config: String,
}

#[derive(Deserialize)]
struct GlobalConfig {
    spi: BTreeMap<String, SpiConfig>,
}

#[derive(Deserialize)]
struct SpiConfig {
    controller: usize,
    fifo_depth: Option<usize>,
    mux_options: BTreeMap<String, SpiMuxOptionConfig>,
    devices: IndexMap<String, DeviceDescriptorConfig>,
}

#[derive(Deserialize)]
struct SpiMuxOptionConfig {
    outputs: Vec<AfPinSetConfig>,
    input: AfPinConfig,
    #[serde(default)]
    swap_data: bool,
}

#[derive(Copy, Clone, Debug, Deserialize)]
enum ConfigPort {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
}

#[derive(Deserialize)]
struct AfPinSetConfig {
    port: ConfigPort,
    pins: Vec<usize>,
    af: Af,
}

#[derive(Deserialize)]
struct AfPinConfig {
    #[serde(flatten)]
    pc: GpioPinConfig,
    af: Af,
}

#[derive(Clone, Debug, Deserialize)]
struct GpioPinConfig {
    port: ConfigPort,
    pin: usize,
}

#[derive(Deserialize, Debug)]
#[serde(transparent)]
struct Af(usize);

#[derive(Clone, Debug, Deserialize)]
struct DeviceDescriptorConfig {
    mux: String,
    #[serde(default)]
    clock_divider: ClockDivider,
    cs: GpioPinConfig,
}

#[derive(Copy, Clone, Debug, Deserialize)]
enum ClockDivider {
    DIV2,
    DIV4,
    DIV8,
    DIV16,
    DIV32,
    DIV64,
    DIV128,
    DIV256,
}

impl Default for ClockDivider {
    fn default() -> ClockDivider {
        // When this config mechanism was introduced, we had everything set at
        // DIV64 for a ~1.5625 MHz SCK rate.
        Self::DIV64
    }
}

///////////////////////////////////////////////////////////////////////////////
// SPI config code generation.
//
// Our config types, by design, map almost directly onto the structs that the
// SPI driver uses to configure itself. This means we can do the code generation
// process in a separable-and-composable fashion, by implementing
// `quote::ToTokens` for most of the config types.
//
// Each impl defines, in isolation, how code generation works for that part of
// the config. This is most of the code generation implementation; the
// `generate_spi_config` routine is just a wrapper.

fn generate_spi_config(
    config: &BTreeMap<String, SpiConfig>,
    task_config: &SpiTaskConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.get(&task_config.global_config).ok_or_else(|| {
        format!(
            "reference to undefined spi config {}",
            task_config.global_config
        )
    })?;

    let out_dir = std::env::var("OUT_DIR")?;
    let dest_path = std::path::Path::new(&out_dir).join("spi_config.rs");

    let mut out = std::fs::File::create(&dest_path)?;

    writeln!(out, "{}", config.to_token_stream())?;

    drop(out);

    call_rustfmt::rustfmt(&dest_path)?;

    Ok(())
}

impl ToTokens for SpiConfig {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        // Work out the mapping from mux names to indices so we can dereference
        // the mux names used in devices.
        let mux_indices: BTreeMap<_, usize> = self
            .mux_options
            .keys()
            .enumerate()
            .map(|(i, k)| (k, i))
            .collect();

        // The svd2rust PAC can't decide whether acronyms are words, so we get
        // to produce both identifiers.
        let devname: syn::Ident =
            syn::parse_str(&format!("SPI{}", self.controller)).unwrap();
        let pname: syn::Ident =
            syn::parse_str(&format!("Spi{}", self.controller)).unwrap();

        // We don't derive ToTokens for DeviceDescriptorConfig because it needs
        // extra knowledge (the mux_indices map) to do the conversion. Instead,
        // convert it here:
        let device_code = self.devices.values().map(|dev| {
            let mux_index = mux_indices[&dev.mux];
            let cs = &dev.cs;
            let div: syn::Ident =
                syn::parse_str(&format!("{:?}", dev.clock_divider)).unwrap();
            quote::quote! {
                DeviceDescriptor {
                    mux_index: #mux_index,
                    cs: #cs,
                    // `spi1` here is _not_ a typo/oversight, the PAC calls all
                    // SPI types spi1.
                    clock_divider: device::spi1::cfg1::MBR_A::#div,
                }
            }
        });

        let muxes = self.mux_options.values();

        // If the user does not specify a fifo depth, we default to the
        // _minimum_ on any SPI block on the STM32H7, which is 8.
        let fifo_depth = self.fifo_depth.unwrap_or(8);

        tokens.append_all(quote::quote! {
            const FIFO_DEPTH: usize = #fifo_depth;
            const CONFIG: ServerConfig = ServerConfig {
                registers: device::#devname::ptr(),
                peripheral: sys_api::Peripheral::#pname,
                mux_options: &[ #(#muxes),* ],
                devices: &[ #(#device_code),* ],
            };
        });
    }
}

impl ToTokens for SpiMuxOptionConfig {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let outputs = &self.outputs;
        let input = &self.input;
        let swap_data = self.swap_data;
        tokens.append_all(quote::quote! {
            SpiMuxOption {
                outputs: &[ #(#outputs),* ],
                input: #input,
                swap_data: #swap_data,
            }
        });
    }
}

impl ToTokens for ConfigPort {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let port: syn::Ident = syn::parse_str(&format!("{:?}", self)).unwrap();
        tokens.append_all(quote::quote! {
            sys_api::Port::#port
        });
    }
}

impl ToTokens for AfPinSetConfig {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let port = self.port;
        let pins = self.pins.iter().map(|pin| {
            quote::quote! {
                (1 << #pin)
            }
        });
        let af = &self.af;
        tokens.append_all(quote::quote! {
            (
                PinSet {
                    port: #port,
                    pin_mask: #( #pins )|*,
                },
                #af,
            )
        });
    }
}

impl ToTokens for AfPinConfig {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let pc = &self.pc;
        let af = &self.af;
        tokens.append_all(quote::quote! {
            (
                #pc,
                #af,
            )
        });
    }
}

impl ToTokens for GpioPinConfig {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let port = &self.port;
        let pin = self.pin;
        tokens.append_all(quote::quote! {
            PinSet {
                port: #port,
                pin_mask: 1 << #pin,
            }
        });
    }
}

impl ToTokens for Af {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let name: syn::Ident =
            syn::parse_str(&format!("AF{}", self.0)).unwrap();
        tokens.append_all(quote::quote! {
            sys_api::Alternate::#name
        });
    }
}

///////////////////////////////////////////////////////////////////////////////
// Check routines.

fn check_spi_config(
    config: &BTreeMap<String, SpiConfig>,
    task_config: &SpiTaskConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // We only want to look at the subset of global configuration relevant to
    // this task, so that error reporting is more focused.
    let config = config.get(&task_config.global_config).ok_or_else(|| {
        format!(
            "reference to undefined spi config {}",
            task_config.global_config
        )
    })?;

    if config.controller < 1 || config.controller > 6 {
        return Err(format!(
            "bad controller {}, valid values are 1 thru 6",
            config.controller
        )
        .into());
    }

    for mux in config.mux_options.values() {
        for out in &mux.outputs {
            check_afpinset(out)?;
        }
        check_afpin(&mux.input)?;
    }

    for (devname, dev) in &config.devices {
        if !config.mux_options.contains_key(&dev.mux) {
            return Err(format!(
                "device {} names undefined mux {}",
                devname, dev.mux
            )
            .into());
        }
        check_gpiopin(&dev.cs)?;
    }

    Ok(())
}

fn check_afpinset(
    config: &AfPinSetConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    for &pin in &config.pins {
        if pin > 15 {
            return Err(format!(
                "pin {:?}{} is invalid, pins are numbered 0-15",
                config.port, pin
            )
            .into());
        }
    }
    if config.af.0 > 15 {
        return Err(format!(
            "af {:?} is invalid, functions are numbered 0-15",
            config.af
        )
        .into());
    }
    Ok(())
}

fn check_afpin(config: &AfPinConfig) -> Result<(), Box<dyn std::error::Error>> {
    check_gpiopin(&config.pc)?;
    if config.af.0 > 15 {
        return Err(format!(
            "af {:?} is invalid, functions are numbered 0-15",
            config.af
        )
        .into());
    }
    Ok(())
}

fn check_gpiopin(
    config: &GpioPinConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    if config.pin > 15 {
        return Err(format!(
            "pin {:?}{} is invalid, pins are numbered 0-15",
            config.port, config.pin
        )
        .into());
    }
    Ok(())
}
