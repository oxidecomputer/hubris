// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use anyhow::Context;
use quote::{quote, TokenStreamExt};
use serde::Deserialize;
use std::{collections::BTreeMap, io::Write};

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct SysConfig {
    /// EXTI interrupts
    #[serde(default)]
    gpio_irqs: BTreeMap<String, GpioIrqConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct GpioIrqConfig {
    port: Port,
    pin: usize,
    owner: GpioIrqOwner,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct GpioIrqOwner {
    name: String,
    notification: String,
}

macro_rules! to_tokens_enum {
    ($(#[$m:meta])* enum $Enum:ident { $($Variant:ident),* }) => {
        $(#[$m])*
        enum $Enum {
            $($Variant),*
        }

        impl quote::ToTokens for $Enum {
            fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
                use proc_macro2::{Ident, Punct, Spacing, Span};
                match self {
                    $(Self::$Variant => {
                        tokens.append(Ident::new(stringify!($Enum), Span::call_site()));
                        tokens.append(Punct::new(':', Spacing::Joint));
                        tokens.append(Punct::new(':', Spacing::Alone));
                        tokens.append(Ident::new(stringify!($Variant), Span::call_site()));
                    }),*
                }
            }
        }
    };
}

to_tokens_enum! {
    #[derive(Copy, Clone, Debug, Deserialize)]
    enum Port {
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
        K
    }
}

pub fn build_gpio_irq_pins() -> anyhow::Result<()> {
    let out_dir = build_util::out_dir();
    let dest_path = out_dir.join("gpio_irq_pins.rs");
    let mut out = std::fs::File::create(&dest_path).with_context(|| {
        format!("failed to create file '{}'", dest_path.display())
    })?;

    let Some(sys_config) =
        build_util::other_task_full_config::<SysConfig>("sys")?.config
    else {
        // No GPIO IRQs are configured; nothing left to do here!
        return Ok(());
    };

    let task = build_util::task_name();
    let pins = sys_config
        .gpio_irqs
        .iter()
        .filter_map(|(name, cfg)| {
            let &GpioIrqConfig {
                pin,
                port,
                ref owner,
            } = cfg;
            // Only generate constants for pins owned by the current task.
            if owner.name != task {
                return None;
            }

            let name = match to_const_name(name.clone()) {
                Ok(name) => name,
                Err(e) => return Some(Err(e)),
            };

            Some(Ok(quote! {
                pub const #name: PinSet = #port.pin(#pin);
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Don't generate an empty module if there are no pins.
    if pins.is_empty() {
        return Ok(());
    }

    let tokens = quote! {
        pub mod gpio_irq_pins {
            use drv_stm32xx_gpio_common::{PinSet, Port};
            #( #pins )*
        }
    };
    writeln!(out, "{tokens}")?;

    Ok(())
}

impl SysConfig {
    pub fn load() -> anyhow::Result<Self> {
        Ok(build_util::task_maybe_config::<Self>()?.unwrap_or_default())
    }

    pub fn needs_exti(&self) -> bool {
        !self.gpio_irqs.is_empty()
    }

    pub fn generate_exti_config(
        &self,
    ) -> anyhow::Result<proc_macro2::TokenStream> {
        #[derive(Debug)]
        struct DispatchEntry {
            port: Port,
            task: syn::Ident,
            note: syn::Ident,
            name: syn::Ident,
        }

        const NUM_EXTI_IRQS: usize = 16;
        let mut dispatch_table: [Option<DispatchEntry>; NUM_EXTI_IRQS] =
            Default::default();
        let mut has_any_notifications = false;

        for (
            name,
            &GpioIrqConfig {
                port,
                pin,
                ref owner,
            },
        ) in &self.gpio_irqs
        {
            match dispatch_table.get_mut(pin as usize) {
                Some(Some(curr)) => {
                    anyhow::bail!("pin {pin} is already mapped to IRQ {curr:?}")
                }
                Some(slot) => {
                    has_any_notifications = true;
                    let task = syn::parse_str(&owner.name)?;
                    let note = quote::format_ident!(
                        "{}_MASK",
                        to_const_name(owner.notification.clone())?
                    );

                    let name = quote::format_ident!(
                        "{}",
                        name.replace('-', "_")
                    );
                    *slot = Some(DispatchEntry {
                        name,
                        port,
                        task,
                        note,
                    })
                }
                None => anyhow::bail!(
                    "GPIO IRQ pin numbers must be < {NUM_EXTI_IRQS}; {pin} is out of range"
                ),
            }
        }

        let dispatches = dispatch_table.iter().map(|slot| match slot {
            Some(DispatchEntry {
                name,
                port,
                task,
                note,
                ..
            }) => quote! {
                Some(ExtiDispatch {
                    port: #port,
                    task: userlib::TaskId::for_index_and_gen(
                        hubris_num_tasks::Task::#task as usize,
                        userlib::Generation::ZERO,
                    ),
                    mask: crate::notifications::#task::#note,
                    name: ExtiIrq::#name,
                })
            },
            None => quote! { None },
        });

        let counter_type = if has_any_notifications {
            let irq_names = dispatch_table
                .iter()
                .filter_map(|slot| Some(&slot.as_ref()?.name));
            quote! {
                #[derive(Copy, Clone, PartialEq, Eq, counters::Count)]
                #[allow(nonstandard_style)]
                pub(crate) enum ExtiIrq {
                    #( #irq_names ),*
                }
            }
        } else {
            // If there are no EXTI notifications enabled, just use `()` as the counter
            // type, as it does implement `counters::Count`, but has no values, so
            // we don't get a "matching on an uninhabited type" error.
            quote! {
                pub(crate) type ExtiIrq = ();
            }
        };

        Ok(quote! {
            #counter_type

            pub(crate) const EXTI_DISPATCH_TABLE: [Option<ExtiDispatch>; #NUM_EXTI_IRQS] = [
                #( #dispatches ),*
            ];
        })
    }
}

fn to_const_name(mut s: String) -> anyhow::Result<syn::Ident> {
    s.make_ascii_uppercase();
    let s = s.replace("-", "_");
    syn::parse_str::<syn::Ident>(&s)
        .with_context(|| format!("`{s}` is not a valid Rust identifier"))
}
