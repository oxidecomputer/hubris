// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;

use anyhow::{bail, Context, Result};
use build_kconfig::{
    InterruptConfig, KernelConfig, OwnedAddress, RegionAttributes,
    RegionConfig, SpecialRole,
};
use indexmap::IndexMap;
use proc_macro2::TokenStream;

fn main() -> Result<()> {
    build_util::expose_m_profile()?;

    println!("cargo::rustc-check-cfg=cfg(hubris_phantom_svc_mitigation)");
    if build_util::target().starts_with("thumbv6m") {
        // Force SVC checks on for v6-M.
        println!("cargo:rustc-cfg=hubris_phantom_svc_mitigation");
    }

    let g = process_config()?;
    generate_statics(&g)?;

    Ok(())
}

struct Generated {
    tasks: Vec<TokenStream>,
    regions: Vec<TokenStream>,
    irq_code: TokenStream,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
enum RegionKey {
    Null,
    Shared(String),
    Owned {
        /// Index of the task, based on ordering in the kconfig
        task_index: usize,

        /// Index of this particular region within the task
        ///
        /// Each task's memory span can be built from multiple contiguous MPU
        /// regions; if that's the case, then `chunk_index` varies.  The region
        /// with `chunk_index == 0` is at the base address.
        chunk_index: usize,

        /// Name of memory which we're using for this region
        memory_name: String,
    },
}

fn process_config() -> Result<Generated> {
    let kconfig: KernelConfig =
        ron::de::from_str(&build_util::env_var("HUBRIS_KCONFIG")?)
            .context("parsing kconfig from HUBRIS_KCONFIG")?;

    // The kconfig data structure keeps things somewhat abstract to give us, the
    // kernel, more freedom about our internal implementation choices. However,
    // this means we have to do some preprocessing before it's useful.

    // The kernel currently uses a flat region descriptor table, and tasks get
    // pointers into that table. Let's assemble that flat table. By putting the
    // regions into an IndexMap, we make their locations findable (using a
    // RegionKey) while also creating a predictable ordering.

    let mut region_table = IndexMap::new();

    // We reserve the first entry (index 0) for the "null" region. This gives no
    // access, and plays the important role of intercepting null pointer
    // dereferences _in the kernel._
    region_table.insert(
        RegionKey::Null,
        RegionConfig {
            base: 0,
            size: 32,
            attributes: RegionAttributes {
                read: false,
                write: false,
                execute: false,
                special_role: None,
            },
        },
    );

    // We'll do the shared bits next.
    for (name, region) in &kconfig.shared_regions {
        region_table.insert(RegionKey::Shared(name.clone()), *region);
    }

    // Finally, the task-specific regions.
    for (i, task) in kconfig.tasks.iter().enumerate() {
        for (name, region) in &task.owned_regions {
            let mut base = region.base;
            for (j, &size) in region.sizes.iter().enumerate() {
                let r = RegionConfig {
                    base,
                    size,
                    attributes: region.attributes,
                };
                base += size;
                region_table.insert(
                    RegionKey::Owned {
                        task_index: i,
                        chunk_index: j,
                        memory_name: name.clone(),
                    },
                    r,
                );
            }
        }
    }

    // We are done mutating this.
    let region_table = region_table;

    // Now, generate the TaskDesc literals. These rely on the region table
    // because they address it by index at the moment.
    let mut task_descs = vec![];

    for (i, task) in kconfig.tasks.iter().enumerate() {
        // Work out the region indices for each of this task's regions.
        let mut regions = vec![
            // Always include the null region.
            region_table.get_index_of(&RegionKey::Null).unwrap(),
        ];

        for (name, region) in &task.owned_regions {
            for j in 0..region.sizes.len() {
                regions.push(
                    region_table
                        .get_index_of(&RegionKey::Owned {
                            task_index: i,
                            chunk_index: j,
                            memory_name: name.clone(),
                        })
                        .unwrap(),
                );
            }
        }

        for name in &task.shared_regions {
            regions.push(
                region_table
                    .get_index_of(&RegionKey::Shared(name.clone()))
                    .with_context(|| {
                        format!("task {i} uses unknown device {name}")
                    })?,
            );
        }

        if regions.len() > 8 {
            bail!("too many regions ({}) for task {i}", regions.len());
        }
        regions.resize(8, 0usize);

        // Order the task's regions in ascending address order.
        //
        // THIS IS IMPORTANT. The kernel exploits this property to do cheaper
        // access tests.
        regions.sort_by_key(|i| region_table.get_index(*i).unwrap().1.base);

        // Translate abstract addresses in the task description into concrete
        // addresses.
        let entry_point =
            translate_address(&region_table, i, task.entry_point.clone());
        let initial_stack =
            translate_address(&region_table, i, task.initial_stack.clone());

        let index = u16::try_from(i).expect("over 2**16 tasks??");
        let priority = task.priority;
        let flags = if task.start_at_boot {
            quote::quote! { TaskFlags::START_AT_BOOT }
        } else {
            quote::quote! { TaskFlags::empty() }
        };
        task_descs.push(quote::quote! {
            TaskDesc {
                regions: [#(&HUBRIS_REGION_DESCS[#regions]),*],
                entry_point: #entry_point,
                initial_stack: #initial_stack,
                priority: #priority,
                index: #index,
                flags: #flags,
            }
        });
    }

    let region_descs = region_table
        .into_iter()
        .map(|(_k, region)| fmt_region(&region))
        .collect();

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
        .map(|(&k, &v)| (k, v))
        .collect::<Vec<_>>();

    let mut per_task_irqs: HashMap<_, Vec<_>> = HashMap::new();
    for (irq, cfg) in &kconfig.irqs {
        let o = abi::InterruptOwner {
            task: cfg.task_index as u32,
            notification: cfg.notification,
        };
        per_task_irqs.entry(o).or_default().push(*irq)
    }
    let task_irq_map = per_task_irqs.into_iter().collect::<Vec<_>>();

    let target = build_util::target();
    let irq_code = if target.starts_with("thumbv6m") {
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

        quote::quote! {
            pub const HUBRIS_IRQ_TASK_LOOKUP:
                phash::SortedList<'_, abi::InterruptNum, abi::InterruptOwner>
                = #irq_task_literal;
            pub const HUBRIS_TASK_IRQ_LOOKUP:
                phash::SortedList<'_,
                abi::InterruptOwner,
                &'static [abi::InterruptNum],
                > = #task_irq_literal;
        }
    } else if target.starts_with("thumbv7m")
        || target.starts_with("thumbv7em")
        || target.starts_with("thumbv8m")
    {
        // First, try to build it as a single-level perfect hash map, which is
        // cheaper but won't always succeed.
        let map1 = if let Ok(task_irq_map) =
            phash_gen::OwnedPerfectHashMap::build(task_irq_map.clone())
        {
            let map_literal =
                fmt_perfect_hash_map(&task_irq_map, fmt_opt_task_irq);
            quote::quote! {
                pub const HUBRIS_TASK_IRQ_LOOKUP:
                    phash::PerfectHashMap<
                    '_,
                    abi::InterruptOwner,
                    &'static [abi::InterruptNum],
                    > = #map_literal;
            }
        } else {
            // Single-level perfect hash failed, make it work with a nested map.
            let task_irq_map =
                phash_gen::OwnedNestedPerfectHashMap::build(task_irq_map)
                    .context("building task-to-IRQ perfect hash")?;
            let task_irq_literal =
                fmt_nested_perfect_hash_map(&task_irq_map, fmt_opt_task_irq);
            quote::quote! {
                pub const HUBRIS_TASK_IRQ_LOOKUP:
                    phash::NestedPerfectHashMap<
                    abi::InterruptOwner,
                    &'static [abi::InterruptNum],
                    > = #task_irq_literal;
            }
        };

        // And now repeat the process for the IRQ-to-task direction.
        let map2 = if let Ok(irq_task_map) =
            phash_gen::OwnedPerfectHashMap::build(irq_task_map.clone())
        {
            let map_literal =
                fmt_perfect_hash_map(&irq_task_map, fmt_opt_irq_task);
            quote::quote! {
                pub const HUBRIS_IRQ_TASK_LOOKUP:
                    phash::PerfectHashMap<
                    '_,
                    abi::InterruptNum,
                    abi::InterruptOwner,
                    > = #map_literal;
            }
        } else {
            let irq_task_map =
                phash_gen::OwnedNestedPerfectHashMap::build(irq_task_map)
                    .context("building IRQ-to-task perfect hash")?;
            let map_literal =
                fmt_nested_perfect_hash_map(&irq_task_map, fmt_opt_irq_task);
            quote::quote! {
                pub const HUBRIS_IRQ_TASK_LOOKUP:
                    phash::NestedPerfectHashMap<
                    abi::InterruptNum,
                    abi::InterruptOwner,
                    > = #map_literal;
            }
        };

        quote::quote! {
            #map1
            #map2
        }
    } else {
        panic!("Don't know the target {target}");
    };

    Ok(Generated {
        tasks: task_descs,
        regions: region_descs,
        irq_code,
    })
}

fn translate_address(
    region_table: &IndexMap<RegionKey, RegionConfig>,
    task_index: usize,
    address: OwnedAddress,
) -> u32 {
    // Addresses within a particular task's memory span can be calculated from
    // the base address of the task's first memory region, which has a
    // chunk_index of 0 (since all chunks within the span are contiguous).
    let key = RegionKey::Owned {
        task_index,
        chunk_index: 0,
        memory_name: address.region_name,
    };
    region_table[&key].base + address.offset
}

fn fmt_region(region: &RegionConfig) -> TokenStream {
    let RegionConfig {
        base,
        size,
        attributes,
    } = region;

    let mut atts = vec![];
    if attributes.read {
        atts.push(quote::quote! { READ });
    }
    if attributes.write {
        atts.push(quote::quote! { WRITE });
    }
    if attributes.execute {
        atts.push(quote::quote! { EXECUTE });
    }
    if let Some(role) = attributes.special_role {
        atts.push(match role {
            SpecialRole::Device => quote::quote! { DEVICE },
            SpecialRole::Dma => quote::quote! { DMA },
        });
    }

    let atts = if atts.is_empty() {
        quote::quote! { RegionAttributes::empty() }
    } else {
        // We have to do the OR-ing on bits and then from_bits_unchecked it
        // because these operations are const, while OR-ing of the
        // RegionAttributes type itself is not (pending const impl stability)
        quote::quote! {
            unsafe {
                RegionAttributes::from_bits_unchecked(
                    #(RegionAttributes::#atts.bits())|*
                )
            }
        }
    };

    quote::quote! {
        RegionDesc {
            base: #base,
            size: #size,
            attributes: #atts,
            arch_data: crate::arch::compute_region_extension_data(
                #base, #size, #atts,
            ),
        }
    }
}

fn generate_statics(gen: &Generated) -> Result<()> {
    let image_id: u64 = build_util::env_var("HUBRIS_IMAGE_ID")?
        .parse()
        .context("parsing HUBRIS_IMAGE_ID")?;

    let out = build_util::out_dir();
    let kconfig_path = out.join("kconfig.rs");
    let mut file =
        File::create(&kconfig_path).context("creating kconfig.rs")?;

    writeln!(file, "// See build.rs for details")?;

    /////////////////////////////////////////////////////////
    // Basic constants and empty space

    let task_count = gen.tasks.len();
    writeln!(
        file,
        "{}",
        quote::quote! {
            const HUBRIS_TASK_COUNT: usize = #task_count;
            #[no_mangle]
            pub static HUBRIS_IMAGE_ID: u64 = #image_id;

            pub(crate) static mut HUBRIS_TASK_TABLE_SPACE:
                core::mem::MaybeUninit<[crate::task::Task; HUBRIS_TASK_COUNT]> =
                core::mem::MaybeUninit::uninit();
        },
    )?;

    /////////////////////////////////////////////////////////
    // Task descriptors

    let task_descs = &gen.tasks;
    writeln!(
        file,
        "{}",
        quote::quote! {
            static HUBRIS_TASK_DESCS: [TaskDesc; HUBRIS_TASK_COUNT] = [
                #(#task_descs,)*
            ];

        },
    )?;

    /////////////////////////////////////////////////////////
    // Region descriptors

    let regions = &gen.regions;
    let region_count = regions.len();
    writeln!(
        file,
        "{}",
        quote::quote! {
            static HUBRIS_REGION_DESCS: [RegionDesc; #region_count] = [
                #(#regions,)*
            ];
        },
    )?;

    /////////////////////////////////////////////////////////
    // Interrupt table

    writeln!(file, "{}", gen.irq_code)?;

    drop(file);
    call_rustfmt::rustfmt(kconfig_path)?;

    Ok(())
}

fn fmt_opt_task_irq(
    v: Option<&(abi::InterruptOwner, Vec<u32>)>,
) -> TokenStream {
    match v {
        Some((owner, irqs)) => fmt_task_irq(owner, irqs),
        None => quote::quote! {
            (abi::InterruptOwner::invalid(), &[])
        },
    }
}

fn fmt_task_irq(owner: &abi::InterruptOwner, irqs: &Vec<u32>) -> TokenStream {
    let (task, not) = (owner.task, owner.notification);
    quote::quote! {
        (
            abi::InterruptOwner { task: #task, notification: #not },
            &[#(abi::InterruptNum(#irqs)),*],
        )
    }
}

fn fmt_opt_irq_task(v: Option<&(u32, InterruptConfig)>) -> TokenStream {
    match v {
        Some((irq, owner)) => fmt_irq_task(irq, owner),
        None => quote::quote! {
            (abi::InterruptNum::invalid(), abi::InterruptOwner::invalid())
        },
    }
}

fn fmt_irq_task(irq: &u32, owner: &InterruptConfig) -> TokenStream {
    let (task, not) = (owner.task_index as u32, owner.notification);
    quote::quote! {
        (
            abi::InterruptNum(#irq),
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
