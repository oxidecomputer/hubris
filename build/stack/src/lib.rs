// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};

/// Estimates the maximum stack size for the given task
///
/// This does not take dynamic function calls into account, which could cause
/// underestimation.  Overestimation is less likely, but still may happen if
/// there are logically impossible call trees (e.g. `A -> B` and `B -> C`, but
/// `B` never calls `C` if called by `A`).
pub fn get_max_stack(
    elf: &Path,
    task_name: &str,
    verbose: bool,
) -> Result<Vec<(u64, String)>> {
    // Open the statically-linked ELF file
    let data = std::fs::read(elf).context("could not open ELF file")?;
    let elf = goblin::elf::Elf::parse(&data)?;

    // Read the .stack_sizes section, which is an array of
    // `(address: u32, stack size: unsigned leb128)` tuples
    let sizes = elf::get_section_by_name(&elf, ".stack_sizes")
        .context("could not get .stack_sizes")?;
    let mut sizes = &data[sizes.sh_offset as usize..][..sizes.sh_size as usize];
    let mut addr_to_frame_size = BTreeMap::new();
    while !sizes.is_empty() {
        let (addr, rest) = sizes.split_at(4);
        let addr = u32::from_le_bytes(addr.try_into().unwrap());
        sizes = rest;
        let size = leb128::read::unsigned(&mut sizes)?;
        addr_to_frame_size.insert(addr, size);
    }

    // There are `$t` and `$d` symbols which indicate the beginning of text
    // versus data in the `.text` region.  We collect them into a `BTreeMap`
    // here so that we can avoid trying to decode inline data words.
    let mut text_regions = BTreeMap::new();
    for sym in elf.syms.iter() {
        if sym.st_name == 0
            || sym.st_size != 0
            || sym.st_type() != goblin::elf::sym::STT_NOTYPE
        {
            continue;
        }

        let addr = sym.st_value as u32;
        let is_text = match elf.strtab.get_at(sym.st_name) {
            Some("$t") => true,
            Some("$d") => false,
            Some(_) => continue,
            None => {
                bail!("bad symbol in {task_name}: {}", sym.st_name);
            }
        };
        text_regions.insert(addr, is_text);
    }
    let is_code = |addr| {
        let mut iter = text_regions.range(..=addr);
        *iter.next_back().unwrap().1
    };

    // We'll be packing everything into this data structure
    #[derive(Debug)]
    struct FunctionData {
        name: String,
        short_name: String,
        frame_size: Option<u64>,
        calls: BTreeSet<u32>,
    }

    let text = elf::get_section_by_name(&elf, ".text")
        .context("could not get .text")?;

    use capstone::{
        Capstone, InsnGroupId, InsnGroupType,
        arch::{ArchOperand, BuildsCapstone, BuildsCapstoneExtraMode, arm},
    };
    let cs = Capstone::new()
        .arm()
        .mode(arm::ArchMode::Thumb)
        .extra_mode(std::iter::once(arm::ArchExtraMode::MClass))
        .detail(true)
        .build()
        .map_err(|e| anyhow!("failed to initialize disassembler: {e:?}"))?;

    // Disassemble each function, building a map of its call sites
    let mut fns = BTreeMap::new();
    for sym in elf.syms.iter() {
        // We only care about named function symbols here
        if sym.st_name == 0 || !sym.is_function() || sym.st_size == 0 {
            continue;
        }

        let Some(name) = elf.strtab.get_at(sym.st_name) else {
            bail!("bad symbol in {task_name}: {}", sym.st_name);
        };

        // Clear the lowest bit, which indicates that the function contains
        // thumb instructions (always true for our systems!)
        let val = sym.st_value & !1;
        let base_addr = val as u32;

        // Get the text region for this function
        let offset = (val - text.sh_addr + text.sh_offset) as usize;
        let text = &data[offset..][..sym.st_size as usize];

        // Split the text region into instruction-only chunks
        let mut chunks = vec![];
        let mut chunk = None;
        for (i, b) in text.iter().enumerate() {
            let addr = base_addr + i as u32;
            if is_code(addr) {
                chunk.get_or_insert((addr, vec![])).1.push(*b);
            } else {
                chunks.extend(chunk.take());
            }
        }
        chunks.extend(chunk); // don't forget the trailing chunk!

        let frame_size = addr_to_frame_size.get(&base_addr).copied();
        let mut calls = BTreeSet::new();
        for (addr, chunk) in chunks {
            let instrs = cs
                .disasm_all(&chunk, addr.into())
                .map_err(|e| anyhow!("disassembly failed: {e:?}"))?;
            for (i, instr) in instrs.iter().enumerate() {
                let detail = cs.insn_detail(instr).map_err(|e| {
                    anyhow!("could not get instruction details: {e}")
                })?;

                // Detect tail calls, which are jumps at the final instruction
                // when the function itself has no stack frame.
                let can_tail = frame_size == Some(0) && i == instrs.len() - 1;
                if detail.groups().iter().any(|g| {
                    g == &InsnGroupId(InsnGroupType::CS_GRP_CALL as u8)
                        || (g == &InsnGroupId(InsnGroupType::CS_GRP_JUMP as u8)
                            && can_tail)
                }) {
                    let arch = detail.arch_detail();
                    let ops = arch.operands();
                    let op = ops.last().unwrap_or_else(|| {
                        panic!("missing operand!");
                    });

                    let ArchOperand::ArmOperand(op) = op else {
                        panic!("bad operand type: {op:?}");
                    };
                    // We can't resolve indirect calls, alas
                    let arm::ArmOperandType::Imm(target) = op.op_type else {
                        continue;
                    };
                    let target = u32::try_from(target).unwrap();

                    // Avoid recursive calls into the same function (or midway
                    // into the function, which is a thing we've seen before!
                    // it's weird!)
                    if !(base_addr..base_addr + sym.st_size as u32)
                        .contains(&target)
                    {
                        calls.insert(target);
                    }
                }
            }
        }

        let name = rustc_demangle::demangle(name).to_string();

        // Strip the trailing hash from the name for ease of printing
        let short_name = if let Some(i) = name.rfind("::") {
            &name[..i]
        } else {
            &name
        }
        .to_owned();

        fns.insert(
            base_addr,
            FunctionData {
                name,
                short_name,
                frame_size,
                calls,
            },
        );
    }

    fn recurse(
        call_stack: &mut Vec<u32>,
        recurse_depth: usize,
        mut stack_depth: u64,
        fns: &BTreeMap<u32, FunctionData>,
        deepest: &mut Option<(u64, Vec<u32>)>,
        verbose: bool,
    ) {
        let addr = *call_stack.last().unwrap();
        let Some(f) = fns.get(&addr) else {
            panic!("found jump to unknown function at {call_stack:08x?}");
        };
        let frame_size = f.frame_size.unwrap_or(0);
        stack_depth += frame_size;
        if verbose {
            let indent = recurse_depth * 2;
            println!(
                "  {:indent$}{addr:08x}: {} [+{frame_size} => {stack_depth}]",
                "",
                f.short_name,
                indent = indent
            );
        }

        if deepest
            .as_ref()
            .map(|(max_depth, _)| stack_depth > *max_depth)
            .unwrap_or(true)
        {
            *deepest = Some((stack_depth, call_stack.to_owned()));
        }
        for j in &f.calls {
            if call_stack.contains(j) {
                // Skip recursive / mutually recursive calls, because we can't
                // reason about them.
                continue;
            } else {
                call_stack.push(*j);
                recurse(
                    call_stack,
                    recurse_depth + 1,
                    stack_depth,
                    fns,
                    deepest,
                    verbose,
                );
                call_stack.pop();
            }
        }
    }

    // Find stack sizes by traversing the graph
    if verbose {
        println!("finding stack sizes for {task_name}");
    }
    let start_addr = fns
        .iter()
        .find(|(_addr, v)| v.name.as_str() == "_start")
        .map(|(addr, _v)| *addr)
        .ok_or_else(|| anyhow!("could not find _start"))?;
    let mut deepest = None;
    recurse(&mut vec![start_addr], 0, 0, &fns, &mut deepest, verbose);

    // Check against our configured task stack size
    let Some((_max_depth, max_stack)) = deepest else {
        unreachable!("must have at least one call stack");
    };

    let mut out = vec![];
    for m in max_stack {
        let f = fns.get(&m).unwrap();
        let name = &f.short_name;
        out.push((f.frame_size.unwrap_or(0), name.clone()));
    }
    Ok(out)
}
