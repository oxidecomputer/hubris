// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Result;
use proc_macro2::TokenStream;
use quote::{format_ident, quote, ToTokens, TokenStreamExt};
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

    const MAX_PINS: usize = (1 << 5) + 31 + 1;

    fn index(&self) -> usize {
        (self.port << 5) + self.pin
    }
}

impl ToTokens for Pin {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let (port, pin) = self.get_port_pin();
        let final_pin = format_ident!("PIO{}_{}", port, pin);
        tokens.append_all(quote! {
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
    value: Option<bool>,
    name: Option<String>,
    pint: Option<usize>,
    setup: Option<bool>,
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

#[derive(Copy, Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[repr(u32)]
pub enum PintSlot {
    Slot0 = 0,
    Slot1 = 1,
    Slot2 = 2,
    Slot3 = 3,
    Slot4 = 4,
    Slot5 = 5,
    Slot6 = 6,
    Slot7 = 7,
}

impl TryFrom<usize> for PintSlot {
    type Error = ();

    fn try_from(v: usize) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(PintSlot::Slot0),
            1 => Ok(PintSlot::Slot1),
            2 => Ok(PintSlot::Slot2),
            3 => Ok(PintSlot::Slot3),
            4 => Ok(PintSlot::Slot4),
            5 => Ok(PintSlot::Slot5),
            6 => Ok(PintSlot::Slot6),
            7 => Ok(PintSlot::Slot7),
            _ => Err(()),
        }
    }
}

impl PintSlot {
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn mask(self) -> u32 {
        1u32 << self.index()
    }
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

    fn get_pint_slot(&self, used: &mut u32) -> Option<PintSlot> {
        if let Some(slot_number) = self.pint {
            if self.pin.port > 1 || self.pin.pin > 32 {
                panic!(
                    "Invalid gpio pin for interrupt: port={}, pin={}",
                    self.pin.port, self.pin.pin
                );
            }
            if let Ok(pint_slot) = PintSlot::try_from(slot_number) {
                let mask = pint_slot.mask();
                if (*used & mask) != 0 {
                    panic!(
                        "Duplicate interrupt slot assignment: {:?}",
                        self.pin
                    );
                }
                *used |= mask;
                Some(pint_slot)
            } else {
                panic!("Invalid pint slot number {slot_number}");
            }
        } else {
            None
        }
    }

    fn call_in_setup(&self) -> bool {
        self.setup.unwrap_or(true)
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
        tokens.append_all(quote! {
            AltFn::#alt_num,
            Mode::#mode,
            Slew::#slew,
            Invert::#invert,
            Digimode::#digimode,
            Opendrain::#od,
        });
    }
}

fn pin_init(
    buf: &mut BufWriter<Vec<u8>>,
    p: &PinConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    // Output pins can specify their value, which is set before configuring
    // their output mode (to avoid glitching).
    let pin_tokens = p.pin.to_token_stream();
    if let Some(v) = p.value {
        assert!(
            matches!(p.direction, Some(Direction::Output)),
            "P{}_{}: can only set value for output pins",
            p.pin.port,
            p.pin.pin
        );
        writeln!(
            buf,
            "iocon.set_val({pin_tokens} {});",
            if v { "Value::One" } else { "Value::Zero" }
        )?;
    }
    if let Some(d) = p.direction {
        writeln!(buf, "iocon.set_dir({pin_tokens} Direction::{d:?});")?;
    }
    Ok(())
}

pub fn codegen(pins: Vec<PinConfig>) -> Result<()> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("pin_config.rs");
    let mut file = std::fs::File::create(&dest_path)?;

    let mut used_slots = 0u32;
    let mut top = BufWriter::new(Vec::new());
    let mut middle = BufWriter::new(Vec::new());
    let mut bottom = BufWriter::new(Vec::new());

    // Pins with interrupts need to be named and will have separate config functions
    // that are called by the main setup function.
    // The same pin must have different names if it is used with different
    // configurations. All but one of the conflicting configs should have
    // "setup = false" in their config specifications.
    //
    // The pin configuration source is organized in sections.
    //   - use statements
    //   - fn setup_pins()
    //   - fn setup_$named_pin()
    //   - constants for named pins

    writeln!(&mut top, "use drv_lpc55_gpio_api::*;\n")?;
    writeln!(
        &mut top,
        "fn setup_pins(task : TaskId) -> Result<(), ()> {{"
    )?;
    if pins.iter().any(|p| p.name.is_none() && p.call_in_setup()) {
        // Some pins are initialized inline in setup_pins()
        writeln!(&mut top, "    let iocon = Pins::from(task);\n")?;
    }

    // If a task defines alternate GPIO configurations. Ensure that not more
    // than one of them is called by the setup function.
    let mut conflict = [0usize; Pin::MAX_PINS];
    let conflicts: Vec<String> = pins
        .iter()
        .filter_map(|p| {
            if p.call_in_setup() {
                // Just report the first conflict
                let clash = conflict[p.pin.index()] == 1;
                conflict[p.pin.index()] += 1;
                if clash {
                    let (pin, port) = p.pin.get_port_pin();
                    Some(format!("P{pin}_{port}"))
                } else {
                    None
                }
            } else {
                // Configurations not called from setup_pins() are ok.
                None
            }
        })
        .collect();
    if !conflicts.is_empty() {
        panic!(
            "Conflicting pin configs: {conflicts:?}. Delete or use \
             'name=...' and setup=false'.",
        );
    }

    for p in pins {
        let pin_tokens = p.to_token_stream();
        let pint_slot_config =
            if let Some(slot) = p.get_pint_slot(&mut used_slots) {
                let si = format_ident!("Slot{}", slot.index());
                quote!(Some(PintSlot::#si))
            } else {
                quote!(None)
            };

        let setup_pin_fn = if let Some(name) = p.name.as_ref() {
            let fn_name = format_ident!("setup_{}", name.to_lowercase());
            writeln!(
                &mut middle,
                r#"
                fn {fn_name}(task: TaskId) {{
                    let iocon = Pins::from(task);

                    iocon.iocon_configure({pin_tokens} {pint_slot_config});
                "#,
            )?;
            let _ = pin_init(&mut middle, &p);
            writeln!(&mut middle, "}}")?;
            Some(fn_name)
        } else {
            None
        };

        if p.call_in_setup() {
            if let Some(fn_name) = setup_pin_fn {
                writeln!(&mut top, "{fn_name}(task);")?;
            } else {
                writeln!(
                    &mut top,
                    "{}",
                    quote!(
                        iocon.iocon_configure(#pin_tokens #pint_slot_config);
                    )
                )?;
                let _ = pin_init(&mut top, &p);
            }
        }

        match p.name {
            None => (),
            Some(ref name) => {
                let pin = p.pin.get_port_pin();
                writeln!(&mut bottom, "#[allow(unused)]")?;
                writeln!(
                    &mut bottom,
                    "const {name}: Pin = Pin::PIO{}_{};",
                    pin.0, pin.1
                )?;

                let mut ignore = 0u32;
                if let Some(slot) = p.get_pint_slot(&mut ignore) {
                    writeln!(&mut bottom, "#[allow(unused)]")?;
                    writeln!(
                        &mut bottom,
                        "pub const {name}_PINT_SLOT: PintSlot = PintSlot::Slot{};",
                        slot.index(),
                    )?;
                }
            }
        }
    }

    writeln!(&mut top, "Ok(())")?;
    writeln!(&mut top, "}}")?;

    writeln!(file, "{}", String::from_utf8(top.into_inner()?).unwrap())?;
    writeln!(
        file,
        "\n{}",
        String::from_utf8(middle.into_inner()?).unwrap()
    )?;
    writeln!(
        file,
        "\n{}",
        String::from_utf8(bottom.into_inner()?).unwrap()
    )?;
    call_rustfmt::rustfmt(&dest_path)?;

    Ok(())
}
