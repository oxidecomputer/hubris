// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;

use abi::{InterruptNum, InterruptOwner};
use anyhow::{Context, Result};
use proc_macro2::TokenStream;
use serde::Deserialize;

fn main() -> Result<()> {
    build_util::expose_m_profile();

    generate_consts()?;
    generate_statics()?;

    Ok(())
}

fn generate_consts() -> Result<()> {
    let out = build_util::out_dir();
    let mut const_file = File::create(out.join("consts.rs"))
        .context("creating consts.rs file")?;

    writeln!(
        const_file,
        "// See build.rs for an explanation of this constant"
    )?;

    // EXC_RETURN is used on ARMv8m to return from an exception. This value
    // differs between secure and non-secure in two important ways:
    // bit 6 = S = secure or non-secure stack used
    // bit 0 = ES = the security domain the exception was taken to
    // These need to be consistent! The failure mode is a secure fault
    // otherwise
    let exc_return_value =
        if let Ok(secure) = build_util::env_var("HUBRIS_SECURE") {
            if secure == "0" {
                0xFFFFFFAC_u32
            } else {
                0xFFFFFFED_u32
            }
        } else {
            0xFFFFFFED_u32
        };

    writeln!(
        const_file,
        "{}",
        quote::quote! {
            pub const EXC_RETURN_CONST: u32 = #exc_return_value;
        },
    )?;

    Ok(())
}

fn generate_statics() -> Result<()> {
    let image_id: u64 = build_util::env_var("HUBRIS_IMAGE_ID")?
        .parse()
        .context("parsing HUBRIS_IMAGE_ID")?;
    let kconfig: KernelConfig =
        ron::de::from_str(&build_util::env_var("HUBRIS_KCONFIG")?)
            .context("parsing kconfig from HUBRIS_KCONFIG")?;

    let out = build_util::out_dir();
    let mut file =
        File::create(out.join("kconfig.rs")).context("creating kconfig.rs")?;

    writeln!(file, "// See build.rs for details")?;

    /////////////////////////////////////////////////////////
    // Basic constants and empty space

    let task_count = kconfig.tasks.len();
    writeln!(
        file,
        "{}",
        quote::quote! {
            #[no_mangle]
            pub static HUBRIS_IMAGE_ID: u64 = #image_id;
            const HUBRIS_TASK_COUNT: usize = #task_count;

            static mut HUBRIS_TASK_TABLE_SPACE:
                core::mem::MaybeUninit<[crate::task::Task; HUBRIS_TASK_COUNT]> =
                core::mem::MaybeUninit::uninit();

            static mut HUBRIS_REGION_TABLE_SPACE:
                core::mem::MaybeUninit<
                    [[&'static abi::RegionDesc; abi::REGIONS_PER_TASK]; HUBRIS_TASK_COUNT],
                > = core::mem::MaybeUninit::uninit();
        },
    )?;

    /////////////////////////////////////////////////////////
    // Task descriptors

    let mut task_descs = vec![];
    for task in &kconfig.tasks {
        let abi::TaskDesc {
            regions,
            entry_point,
            initial_stack,
            priority,
            index,
            ..
        } = task;
        let flags_bits = task.flags.bits();
        task_descs.push(quote::quote! {
            abi::TaskDesc {
                regions: [
                    #(#regions,)*
                ],
                entry_point: #entry_point,
                initial_stack: #initial_stack,
                priority: #priority,
                index: #index,
                flags: unsafe { abi::TaskFlags::from_bits_unchecked(#flags_bits) },
            }
        });
    }

    writeln!(
        file,
        "{}",
        quote::quote! {
            static HUBRIS_TASK_DESCS: [abi::TaskDesc; HUBRIS_TASK_COUNT] = [
                #(#task_descs,)*
            ];

        },
    )?;

    /////////////////////////////////////////////////////////
    // Region descriptors

    let mut region_descs = vec![];
    for region in &kconfig.regions {
        let abi::RegionDesc {
            base,
            size,
            attributes,
        } = region;
        let attbits = attributes.bits();
        region_descs.push(quote::quote! {
            abi::RegionDesc {
                base: #base,
                size: #size,
                attributes: unsafe {
                    abi::RegionAttributes::from_bits_unchecked(#attbits)
                },
            }
        });
    }
    let region_count = kconfig.regions.len();
    writeln!(
        file,
        "{}",
        quote::quote! {
            static HUBRIS_REGION_DESCS: [abi::RegionDesc; #region_count] = [
                #(#region_descs,)*
            ];
        },
    )?;

    /////////////////////////////////////////////////////////
    // Interrupt table

    // Now, we generate two mappings:
    //  irq num => abi::Interrupt
    //  (task, notifications) => abi::InterruptSet
    //
    // The first table allows for efficient implementation of the default
    // interrupt handler, which needs to look up the task corresponding with a
    // given interrupt.
    //
    // The second table allows for efficient implementation of `irq_control`,
    // where a task enables or disables one or more IRQS based on notification
    // masks.
    //
    // The form of the mapping will depend on the target architecture, below.
    let irq_task_map = kconfig
        .irqs
        .iter()
        .map(|irq| (irq.irq, irq.owner))
        .collect::<Vec<_>>();

    let mut per_task_irqs: HashMap<_, Vec<_>> = HashMap::new();
    for irq in &kconfig.irqs {
        per_task_irqs.entry(irq.owner).or_default().push(irq.irq)
    }
    let task_irq_map = per_task_irqs.into_iter().collect::<Vec<_>>();

    let target = build_util::target();
    if target.starts_with("thumbv6m") {
        // On ARMv6-M we have no hardware division, which the perfect hash table
        // relies on (to get efficient integer remainder). Fall back to a good
        // old sorted list with binary search instead.
        //
        // This means our dispatch time for interrupts on ARMv6-M is O(log N)
        // instead of O(1), but these parts also tend to have few interrupts,
        // so, not the end of the world.

        let task_irq_map = phash_gen::OwnedSortedList::build(task_irq_map)
            .context("building task-to-IRQ map")?;
        let irq_task_map = phash_gen::OwnedSortedList::build(irq_task_map)
            .context("building IRQ-to-task map")?;

        // Generate text for the Interrupt and InterruptSet tables stored in the
        // sorted lists
        let irq_task_literal = fmt_sorted_list(&irq_task_map, fmt_irq_task);
        let task_irq_literal = fmt_sorted_list(&task_irq_map, fmt_task_irq);

        write!(
            file,
            "{}",
            quote::quote! {
                pub const HUBRIS_IRQ_TASK_LOOKUP:
                    phash::SortedList<abi::InterruptNum, abi::InterruptOwner>
                    = #irq_task_literal;
                pub const HUBRIS_TASK_IRQ_LOOKUP:
                    phash::SortedList<
                        abi::InterruptOwner,
                        &'static [abi::InterruptNum],
                    > = #task_irq_literal;
            }
        )?;
    } else if target.starts_with("thumbv7m")
        || target.starts_with("thumbv7em")
        || target.starts_with("thumbv8m")
    {
        // First, try to build it as a single-level perfect hash map, which is
        // cheaper but won't always succeed.
        if let Ok(task_irq_map) =
            phash_gen::OwnedPerfectHashMap::build(task_irq_map.clone())
        {
            let map_literal =
                fmt_perfect_hash_map(&task_irq_map, fmt_opt_task_irq);
            writeln!(
                file,
                "{}",
                quote::quote! {
                    pub const HUBRIS_TASK_IRQ_LOOKUP:
                        phash::PerfectHashMap<
                            '_,
                            abi::InterruptOwner,
                            &'static [abi::InterruptNum],
                        > = #map_literal;
                },
            )?;
        } else {
            // Single-level perfect hash failed, make it work with a nested map.
            let task_irq_map =
                phash_gen::OwnedNestedPerfectHashMap::build(task_irq_map)
                    .context("building task-to-IRQ perfect hash")?;
            let task_irq_literal =
                fmt_nested_perfect_hash_map(&task_irq_map, fmt_opt_task_irq);
            writeln!(
                file,
                "{}",
                quote::quote! {
                    pub const HUBRIS_TASK_IRQ_LOOKUP:
                        phash::NestedPerfectHashMap<
                            abi::InterruptOwner,
                            &'static [abi::InterruptNum],
                        > = #task_irq_literal;
                },
            )?;
        }

        // And now repeat the process for the IRQ-to-task direction.
        if let Ok(irq_task_map) =
            phash_gen::OwnedPerfectHashMap::build(irq_task_map.clone())
        {
            let map_literal =
                fmt_perfect_hash_map(&irq_task_map, fmt_opt_irq_task);
            writeln!(
                file,
                "{}",
                quote::quote! {
                    pub const HUBRIS_IRQ_TASK_LOOKUP:
                        phash::PerfectHashMap<
                            '_,
                            abi::InterruptNum,
                            abi::InterruptOwner,
                        > = #map_literal;
                },
            )?;
        } else {
            let irq_task_map =
                phash_gen::OwnedNestedPerfectHashMap::build(irq_task_map)
                    .context("building IRQ-to-task perfect hash")?;
            let map_literal =
                fmt_nested_perfect_hash_map(&irq_task_map, fmt_opt_irq_task);
            writeln!(
                file,
                "{}",
                quote::quote! {
                    pub const HUBRIS_IRQ_TASK_LOOKUP:
                        phash::NestedPerfectHashMap<
                            abi::InterruptNum,
                            abi::InterruptOwner,
                        > = #map_literal;
                },
            )?;
        }
    } else {
        panic!("Don't know the target {}", target);
    }

    Ok(())
}

fn fmt_opt_task_irq(
    v: Option<&(InterruptOwner, Vec<InterruptNum>)>,
) -> TokenStream {
    match v {
        Some((owner, irqs)) => fmt_task_irq(owner, irqs),
        None => quote::quote! {
            (abi::InterruptOwner::invalid(), &[])
        },
    }
}

fn fmt_task_irq(
    owner: &InterruptOwner,
    irqs: &Vec<InterruptNum>,
) -> TokenStream {
    let irqs = irqs.iter().map(|irqnum| irqnum.0);
    let (task, not) = (owner.task, owner.notification);
    quote::quote! {
        (
            abi::InterruptOwner { task: #task, notification: #not },
            &[#(abi::InterruptNum(#irqs)),*],
        )
    }
}

fn fmt_opt_irq_task(v: Option<&(InterruptNum, InterruptOwner)>) -> TokenStream {
    match v {
        Some((irq, owner)) => fmt_irq_task(irq, owner),
        None => quote::quote! {
            (abi::InterruptNum::invalid(), abi::InterruptOwner::invalid())
        },
    }
}

fn fmt_irq_task(irq: &InterruptNum, owner: &InterruptOwner) -> TokenStream {
    let (irqnum, task, not) = (irq.0, owner.task, owner.notification);
    quote::quote! {
        (
            abi::InterruptNum(#irqnum),
            abi::InterruptOwner { task: #task, notification: #not },
        )
    }
}

fn fmt_sorted_list<K, V>(
    list: &phash_gen::OwnedSortedList<K, V>,
    element: impl Fn(&K, &V) -> TokenStream,
) -> TokenStream {
    let values = list.values.iter().map(|(k, v)| element(k, v));
    quote::quote! {
        phash::SortedList {
            values: &[#(#values,)*],
        }
    }
}

fn fmt_perfect_hash_map<K, V>(
    map: &phash_gen::OwnedPerfectHashMap<K, V>,
    element: impl Fn(Option<&(K, V)>) -> TokenStream,
) -> TokenStream {
    let values = map.values.iter().map(|o| element(o.as_ref()));
    let m = map.m;
    quote::quote! {
        phash::PerfectHashMap {
            m: #m,
            values: &[#(#values,)*],
        }
    }
}

fn fmt_nested_perfect_hash_map<K, V>(
    map: &phash_gen::OwnedNestedPerfectHashMap<K, V>,
    element: impl Fn(Option<&(K, V)>) -> TokenStream,
) -> TokenStream {
    let values = map.values.iter().map(|v| {
        let inner = v.iter().map(|o| element(o.as_ref()));
        quote::quote! {
            &[#(#inner,)*]
        }
    });
    let m = map.m;
    let g = &map.g;
    quote::quote! {
        phash::NestedPerfectHashMap {
            m: #m,
            g: &[#(#g,)*],
            values: &[#(#values,)*],
        }
    }
}

#[derive(Deserialize)]
struct KernelConfig {
    tasks: Vec<abi::TaskDesc>,
    regions: Vec<abi::RegionDesc>,
    irqs: Vec<abi::Interrupt>,
}
