// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use quote::{quote, TokenStreamExt};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize, Default)]
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
                        owner.notification.to_uppercase().replace('-', "_")
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
