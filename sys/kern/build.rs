// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use serde::Deserialize;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_util::expose_m_profile();

    generate_consts()?;
    generate_statics()?;

    Ok(())
}

fn generate_consts() -> Result<(), Box<dyn std::error::Error>> {
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut const_file = File::create(out.join("consts.rs")).unwrap();

    println!("cargo:rerun-if-env-changed=HUBRIS_SECURE");
    writeln!(
        const_file,
        "// See build.rs for an explanation of this constant"
    )
    .unwrap();
    // EXC_RETURN is used on ARMv8m to return from an exception. This value
    // differs between secure and non-secure in two important ways:
    // bit 6 = S = secure or non-secure stack used
    // bit 0 = ES = the security domain the exception was taken to
    // These need to be consistent! The failure mode is a secure fault
    // otherwise
    if let Ok(secure) = env::var("HUBRIS_SECURE") {
        if secure == "0" {
            writeln!(
                const_file,
                "pub const EXC_RETURN_CONST : u32 = 0xFFFFFFAC;"
            )
            .unwrap();
        } else {
            writeln!(
                const_file,
                "pub const EXC_RETURN_CONST : u32 = 0xFFFFFFED;"
            )
            .unwrap();
        }
    } else {
        writeln!(const_file, "pub const EXC_RETURN_CONST : u32 = 0xFFFFFFED;")
            .unwrap();
    }
    Ok(())
}

fn generate_statics() -> Result<(), Box<dyn std::error::Error>> {
    let image_id: u64 = env::var("HUBRIS_IMAGE_ID")?.parse()?;
    println!("cargo:rerun-if-env-changed=HUBRIS_IMAGE_ID");

    let kconfig: KernelConfig =
        ron::de::from_str(&env::var("HUBRIS_KCONFIG")?)?;
    println!("cargo:rerun-if-env-changed=HUBRIS_KCONFIG");

    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut file = File::create(out.join("kconfig.rs")).unwrap();

    writeln!(file, "// See build.rs for details")?;

    writeln!(file, "#[no_mangle]")?;
    writeln!(file, "pub static HUBRIS_IMAGE_ID: u64 = {};", image_id)?;
    writeln!(
        file,
        "const HUBRIS_FAULT_NOTIFICATION: u32 = {};",
        kconfig.supervisor_notification
    )?;
    writeln!(
        file,
        "const HUBRIS_TASK_COUNT: usize = {};",
        kconfig.tasks.len()
    )?;

    writeln!(
        file,
        "static HUBRIS_TASK_DESCS: [abi::TaskDesc; HUBRIS_TASK_COUNT] = ["
    )?;
    for task in &kconfig.tasks {
        writeln!(file, "    abi::TaskDesc {{")?;
        writeln!(file, "        regions: [")?;
        for region in &task.regions {
            writeln!(file, "            {},", region)?;
        }
        writeln!(file, "        ],")?;
        writeln!(file, "        entry_point: {:#010x},", task.entry_point)?;
        writeln!(file, "        initial_stack: {:#010x},", task.initial_stack)?;
        writeln!(file, "        priority: {},", task.priority)?;
        writeln!(
            file,
            "        flags: unsafe {{ \
            abi::TaskFlags::from_bits_unchecked({}) }},",
            task.flags.bits()
        )?;
        writeln!(file, "    }},")?;
    }
    writeln!(file, "];")?;

    writeln!(
        file,
        "static mut HUBRIS_TASK_TABLE_SPACE: \
        core::mem::MaybeUninit<[crate::task::Task; HUBRIS_TASK_COUNT]> = \
        core::mem::MaybeUninit::uninit();",
    )?;

    writeln!(
        file,
        "static mut HUBRIS_REGION_TABLE_SPACE: \
        core::mem::MaybeUninit<[\
            [&'static abi::RegionDesc; crate::app::REGIONS_PER_TASK]; \
            HUBRIS_TASK_COUNT]> = core::mem::MaybeUninit::uninit();"
    )?;

    writeln!(
        file,
        "static HUBRIS_REGION_DESCS: [abi::RegionDesc; {}] = [",
        kconfig.regions.len()
    )?;
    for region in &kconfig.regions {
        writeln!(file, "    abi::RegionDesc {{")?;
        writeln!(file, "        base: {:#010x},", region.base)?;
        writeln!(file, "        size: {:#010x},", region.size)?;
        writeln!(
            file,
            "        attributes: unsafe {{ \
            abi::RegionAttributes::from_bits_unchecked({}) }},",
            region.attributes.bits()
        )?;
        writeln!(file, "        reserved_zero: 0,")?;
        writeln!(file, "    }},")?;
    }
    writeln!(file, "];")?;

    writeln!(
        file,
        "static HUBRIS_INTERRUPTS: [abi::Interrupt; {}] = [",
        kconfig.irqs.len()
    )?;
    for irq in &kconfig.irqs {
        writeln!(file, "    abi::Interrupt {{")?;
        writeln!(file, "        irq: {},", irq.irq)?;
        writeln!(file, "        task: {},", irq.task)?;
        writeln!(file, "        notification: 0b{:b},", irq.notification)?;
        writeln!(file, "    }},")?;
    }
    writeln!(file, "];")?;

    // Now, we generate two perfect hashes:
    //  irq num => abi::Interrupt
    //  (task, notifications) => abi::InterruptSet
    //
    // The first table allows for efficient implementation of the default
    // interrupt handle, with O(1) lookup of the task which owns a particular
    // interrupt.
    //
    // The second table allows for efficient implementation of `irq_control`,
    // where a task enables or disables one or more IRQS based on notification
    // masks.
    let irq_task_map =
        phash_gen::generate_hash(kconfig.irqs.clone(), |irq| irq.irq).unwrap();

    #[derive(Debug, Eq, PartialEq, Hash)]
    struct OwnedInterruptSet {
        pub task: u32,
        pub notification: u32,
        pub irqs: Vec<u32>,
    }
    let mut per_task_irqs: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    for irq in &kconfig.irqs {
        per_task_irqs
            .entry((irq.task, irq.notification))
            .or_default()
            .push(irq.irq)
    }
    let per_task_irqs = per_task_irqs
        .into_iter()
        .map(|((task, notification), irqs)| OwnedInterruptSet {
            task,
            notification,
            irqs,
        })
        .collect();
    let task_irq_map =
        phash_gen::generate_hash(per_task_irqs, |i| (i.task, i.notification))
            .unwrap();

    // Generate text for the Interrupt and InterruptSet tables stored in the
    // PerfectHashes
    let irq_task_value = irq_task_map
        .values
        .iter()
        .map(|irq| match irq {
            Some(irq) => format!(
                "abi::Interrupt {{ irq: {}, task: {}, notification: 0b{:b} }}",
                irq.irq, irq.task, irq.notification
            ),
            None => {
                "abi::Interrupt { irq: 0, task: u32::MAX, notification: 0 }"
                    .to_string()
            }
        })
        .collect::<Vec<String>>()
        .join(",\n        ");
    let task_irq_value = task_irq_map
        .values
        .iter()
        .map(|irq| match irq {
            Some(irq) => format!(
                "abi::InterruptSet {{ task: {}, notification: 0b{:b}, irqs: &{:?} }}",
                irq.task, irq.notification, irq.irqs
            ),
            None => {
                "abi::InterruptSet { task: u32::MAX, notification: 0, irqs: &[] }"
                    .to_string()
            }
        })
        .collect::<Vec<String>>()
        .join(",\n        ");

    write!(file, "
use phash::PerfectHash;
pub static HUBRIS_IRQ_TASK_LOOKUP: PerfectHash::<u32, abi::Interrupt> = PerfectHash {{
    m: {},
    values: &[
        {},
    ],
}};
pub static HUBRIS_TASK_IRQ_LOOKUP: PerfectHash::<(u32, u32), abi::InterruptSet> = PerfectHash {{
    m: {:?},
    values: &[
        {},
    ],
}};",
        irq_task_map.m, irq_task_value, task_irq_map.m, task_irq_value)?;

    Ok(())
}

#[derive(Deserialize)]
struct KernelConfig {
    tasks: Vec<abi::TaskDesc>,
    regions: Vec<abi::RegionDesc>,
    irqs: Vec<abi::Interrupt>,
    supervisor_notification: u32,
}
