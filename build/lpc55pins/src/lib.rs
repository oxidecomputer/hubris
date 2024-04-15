// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
use proc_macro2::TokenStream;
use quote::{format_ident, ToTokens, TokenStreamExt};
use serde::Deserialize;
use std::io::{BufWriter, Write};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
struct Pin {
    port: usize,
    pin: usize,
}

impl Pin {
    fn get_port_pin(&self) -> (usize, usize) {
        assert!(self.pin < 32, "Invalid pin {}", self.pin);
        assert!(self.port < 2, "Invalid port {}", self.port);

        (self.port, self.pin)
    }
}

impl ToTokens for Pin {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let (port, pin) = self.get_port_pin();
        let final_pin = format_ident!("PIO{}_{}", port, pin);
        tokens.append_all(quote::quote! {
            // Yes we want the trailing comma
            Pin::#final_pin,
        });
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub struct PinConfig {
    pin: Pin,
    alt: usize,
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    slew: Slew,
    #[serde(default)]
    invert: Invert,
    #[serde(default)]
    digimode: Digimode,
    #[serde(default)]
    opendrain: Opendrain,
    direction: Option<Direction>,
    name: Option<String>,
}

#[derive(Copy, Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    #[default]
    NoPull,
    PullDown,
    PullUp,
    Repeater,
}

#[derive(Copy, Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Slew {
    #[default]
    Standard,
    Fast,
}

#[derive(Copy, Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Invert {
    #[default]
    Disable,
    Enabled,
}

#[derive(Copy, Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Digimode {
    #[default]
    Digital,
    Analog,
}

#[derive(Copy, Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Opendrain {
    #[default]
    Normal,
    Opendrain,
}

#[derive(Copy, Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Input,
    Output,
}

impl PinConfig {
    fn get_alt(&self) -> usize {
        if self.alt > 9 {
            panic!("Invalid alt setting {}", self.alt);
        }

        self.alt
    }
}

impl ToTokens for PinConfig {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let final_pin = self.pin.to_token_stream();
        let alt_num = format_ident!("Alt{}", self.get_alt());

        let mode = format_ident!("{}", format!("{:?}", self.mode));
        let slew = format_ident!("{}", format!("{:?}", self.slew));
        let invert = format_ident!("{}", format!("{:?}", self.invert));
        let digimode = format_ident!("{}", format!("{:?}", self.digimode));
        let od = format_ident!("{}", format!("{:?}", self.opendrain));
        tokens.append_all(final_pin);
        tokens.append_all(quote::quote! {
            AltFn::#alt_num,
            Mode::#mode,
            Slew::#slew,
            Invert::#invert,
            Digimode::#digimode,
            Opendrain::#od,
        });
    }
}

pub fn codegen(pins: Vec<PinConfig>) -> Result<()> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("pin_config.rs");
    let mut file = std::fs::File::create(dest_path)?;

    let mut buf = BufWriter::new(Vec::new());
    if pins.iter().any(|p| p.name.is_some()) {
        writeln!(&mut buf, "use drv_lpc55_gpio_api::Pin;")?;
    }
    writeln!(
        &mut file,
        "fn setup_pins(task : TaskId) -> Result<(), ()> {{"
    )?;
    writeln!(&mut file, "use drv_lpc55_gpio_api::*;")?;
    writeln!(&mut file, "let iocon = Pins::from(task);")?;
    for p in pins {
        writeln!(&mut file, "iocon.iocon_configure(")?;
        writeln!(&mut file, "{}", p.to_token_stream())?;
        writeln!(&mut file, ");")?;

        match p.direction {
            None => (),
            Some(d) => {
                writeln!(&mut file, "iocon.set_dir(")?;
                writeln!(&mut file, "{}", p.pin.to_token_stream())?;
                writeln!(&mut file, "Direction::{d:?}")?;
                writeln!(&mut file, ");")?;
            }
        }
        match p.name {
            None => (),
            Some(name) => {
                let pin = p.pin.get_port_pin();
                writeln!(&mut buf, "#[allow(unused)]")?;
                writeln!(
                    &mut buf,
                    "const {}: Pin = Pin::PIO{}_{};",
                    name, pin.0, pin.1
                )?;
            }
        }
    }

    writeln!(&mut file, "Ok(())")?;
    writeln!(&mut file, "}}")?;
    write!(file, "{}", String::from_utf8(buf.into_inner()?).unwrap())?;

    Ok(())
}
