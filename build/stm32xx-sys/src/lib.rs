// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use quote::{quote, TokenStreamExt};
use serde::Deserialize;
use std::collections::BTreeMap;

/// This represents our _subset_ of global config and _must not_ be marked with
/// `deny_unknown_fields`!
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct GlobalConfig {
    sys: SysConfig,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SysConfig {
    /// EXTI interrupts
    gpio_irqs: BTreeMap<String, GpioIrqConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct GpioIrqConfig {
    port: Port,
    pin: u8,
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

impl SysConfig {
    pub fn load() -> anyhow::Result<Self> {
        Ok(build_util::config::<GlobalConfig>()?.sys)
    }

    pub fn needs_exti(&self) -> bool {
        !self.gpio_irqs.is_empty()
    }

    pub fn generate_exti_config(
        &self,
    ) -> anyhow::Result<proc_macro2::TokenStream> {
        #[derive(Debug)]
        struct DispatchEntry<'a> {
            _name: &'a str,
            port: Port,
            task: syn::Ident,
            note: syn::Ident,
        }

        const NUM_EXTI_IRQS: usize = 16;
        let mut dispatch_table: [Option<DispatchEntry<'_>>; NUM_EXTI_IRQS] =
            Default::default();

        for (
            _name,
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
                    let task = syn::parse_str(&owner.name)?;
                    let note = quote::format_ident!(
                        "{}_MASK",
                        owner.notification.to_uppercase().replace('-', "_")
                    );
                    *slot = Some(DispatchEntry {
                        _name,
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
                port, task, note, ..
            }) => quote! {
                Some((
                    #port,
                    userlib::TaskId::for_index_and_gen(
                        hubris_num_tasks::Task::#task as usize,
                        userlib::Generation::ZERO,
                    ),
                    crate::notifications::#task::#note,
                ))
            },
            None => quote! { None },
        });

        Ok(quote! {
            pub(crate) const EXTI_DISPATCH_TABLE: [Option<(Port, TaskId, u32)>; #NUM_EXTI_IRQS] = [
                #( #dispatches ),*
            ];
        })
    }
}
