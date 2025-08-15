// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use indexmap::IndexMap;
use proc_macro2::TokenStream;
use quote::{ToTokens, TokenStreamExt};
use serde::Deserialize;
use std::collections::BTreeMap;

/// This represents our _subset_ of global config and _must not_ be marked with
/// `deny_unknown_fields`!
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SpiGlobalConfig {
    pub spi: BTreeMap<String, SpiConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpiConfig {
    pub controller: usize,
    pub fifo_depth: Option<usize>,
    pub mux_options: BTreeMap<String, SpiMuxOptionConfig>,
    pub devices: IndexMap<String, DeviceDescriptorConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpiMuxOptionConfig {
    pub outputs: Vec<AfPinSetConfig>,
    pub input: AfPinConfig,
    #[serde(default)]
    pub swap_data: bool,
}

#[derive(Copy, Clone, Debug, Deserialize)]
pub enum ConfigPort {
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
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AfPinSetConfig {
    pub port: ConfigPort,
    pub pins: Vec<usize>,
    pub af: Af,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AfPinConfig {
    #[serde(flatten)]
    pub pc: GpioPinConfig,
    pub af: Af,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct GpioPinConfig {
    pub port: ConfigPort,
    pub pin: usize,
}

#[derive(Deserialize, Debug)]
#[serde(transparent)]
pub struct Af(pub usize);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceDescriptorConfig {
    pub mux: String,
    #[serde(default)]
    pub clock_divider: ClockDivider,
    pub cs: Vec<GpioPinConfig>,
}

#[derive(Copy, Clone, Debug, Deserialize)]
pub enum ClockDivider {
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
                    cs: &[ #(#cs),* ],
                    // `spi1` here is _not_ a typo/oversight, the PAC calls all
                    // SPI types spi1.
                    clock_divider: device::spi1::cfg1::MBR_A::#div,
                }
            }
        });

        let device_names = self.devices.keys().enumerate().map(|(i, name)| {
            let name: syn::Ident =
                syn::parse_str(&name.to_uppercase()).unwrap();
            let i: u8 = i.try_into().unwrap();
            quote::quote! { pub const #name: u8 = #i; }
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
            pub mod devices {
                #(#device_names)*
            }
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
        let port: syn::Ident = syn::parse_str(&format!("{self:?}")).unwrap();
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
