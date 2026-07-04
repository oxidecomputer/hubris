// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use capstone::{
    Capstone, InsnGroupId, InsnGroupType,
    arch::{ArchOperand, BuildsCapstone, BuildsCapstoneExtraMode, arm},
};
use goblin::elf::{Elf, SectionHeader, Sym};

// We'll be packing everything into this data structure
#[derive(Debug)]
struct FunctionData {
    name: String,
    short_name: String,
    frame_size: Option<u64>,
    calls: BTreeSet<u32>,
}

////////////////////////////////////////////////////////////////////////////////
// Pulling data from the elf file
////////////////////////////////////////////////////////////////////////////////

/// Read the .stack_sizes section, which is an array of
/// `(address: u32, stack size: unsigned leb128)` tuples.
fn extract_stack_sizes_section(
    raw_elf: &[u8],
    parsed_elf: &Elf<'_>,
) -> Result<BTreeMap<u32, u64>> {
    let sizes = elf::get_section_by_name(parsed_elf, ".stack_sizes")
        .context("could not get .stack_sizes")?;
    let mut sizes =
        &raw_elf[sizes.sh_offset as usize..][..sizes.sh_size as usize];
    let mut addr_to_frame_size = BTreeMap::new();
    while !sizes.is_empty() {
        let (addr, rest) = sizes.split_at(4);
        let addr = u32::from_le_bytes(addr.try_into().unwrap());
        sizes = rest;
        let size = leb128::read::unsigned(&mut sizes)?;
        addr_to_frame_size.insert(addr, size);
    }

    Ok(addr_to_frame_size)
}

// There are `$t` and `$d` symbols which indicate the beginning of text
// versus data in the `.text` region.  We collect them into a `BTreeMap`
// here so that we can avoid trying to decode inline data words.
fn extract_text_regions(
    parsed_elf: &Elf<'_>,
    task_name: &str,
) -> Result<BTreeMap<u32, bool>> {
    let mut text_regions = BTreeMap::new();
    for sym in parsed_elf.syms.iter() {
        if sym.st_name == 0
            || sym.st_size != 0
            || sym.st_type() != goblin::elf::sym::STT_NOTYPE
        {
            continue;
        }

        let addr = sym.st_value as u32;
        let is_text = match parsed_elf.strtab.get_at(sym.st_name) {
            Some("$t") => true,
            Some("$d") => false,
            Some(_) => continue,
            None => {
                bail!("bad symbol in {task_name}: {}", sym.st_name);
            }
        };
        text_regions.insert(addr, is_text);
    }
    Ok(text_regions)
}

struct SymbolItem<'a> {
    sym: Sym,
    name: &'a str,
    base_addr: u64,
    text_region: &'a [u8],
}

struct ChunkItem {
    code: Vec<u8>,
    addr: u64,
}

impl SymbolItem<'_> {
    // TODO: return `Vec<(u32, &[u8])>?
    fn extract_instruction_chunks<F>(&self, is_code: F) -> Vec<ChunkItem>
    where
        F: Fn(u32) -> bool,
    {
        // Split the text region into instruction-only chunks
        let mut chunks = vec![];
        let mut chunk = None;
        for (i, b) in self.text_region.iter().enumerate() {
            let addr = self.base_addr + i as u64;
            if is_code(addr as u32) {
                chunk
                    .get_or_insert(ChunkItem { addr, code: vec![] })
                    .code
                    .push(*b);
            } else if let Some(c) = chunk.take() {
                chunks.push(c);
            }
        }
        chunks.extend(chunk); // don't forget the trailing chunk!
        chunks
    }
}

fn fn_symbol_iter<'a>(
    parsed_elf: &Elf<'a>,
    text_section: &SectionHeader,
    raw_elf: &'a [u8],
) -> impl Iterator<Item = SymbolItem<'a>> {
    parsed_elf
        .syms
        .iter()
        // We only care about named function symbols here
        .filter(|s| s.st_name != 0)
        .filter(|s| s.is_function())
        .filter(|s| s.st_size != 0)
        // TODO: assert?
        .filter_map(|s| {
            // Clear the lowest bit, which indicates that the function contains
            // thumb instructions (always true for our systems!)
            let base_addr = s.st_value & !1;

            // Get the text region for this function
            let offset = (base_addr - text_section.sh_addr
                + text_section.sh_offset) as usize;
            let text_region = &raw_elf[offset..][..s.st_size as usize];

            // Bake into a handy collected symbol item
            Some(SymbolItem {
                sym: s,
                name: parsed_elf.strtab.get_at(s.st_name)?,
                base_addr,
                text_region,
            })
        })
}

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

    // Get sizes of stack frames by addr from the elf
    let addr_to_frame_size = extract_stack_sizes_section(&data, &elf)?;

    let text_regions = extract_text_regions(&elf, task_name)?;
    let is_code = |addr| {
        let mut iter = text_regions.range(..=addr);
        *iter.next_back().unwrap().1
    };

    let text = elf::get_section_by_name(&elf, ".text")
        .context("could not get .text")?;

    let cs = Capstone::new()
        .arm()
        .mode(arm::ArchMode::Thumb)
        .extra_mode(std::iter::once(arm::ArchExtraMode::MClass))
        .detail(true)
        .build()
        .map_err(|e| anyhow!("failed to initialize disassembler: {e:?}"))?;

    // Disassemble each function, building a map of its call sites
    let mut fns = BTreeMap::new();
    for sym_item in fn_symbol_iter(&elf, text, &data) {
        // TODO
        let sym = sym_item.sym;
        let base_addr = sym_item.base_addr as u32;
        let name = sym_item.name;

        // Split the text region into instruction-only chunks
        let chunks = sym_item.extract_instruction_chunks(is_code);

        let frame_size = addr_to_frame_size.get(&base_addr).copied();
        let mut calls = BTreeSet::new();
        for chunk_item in chunks {
            let instrs = cs
                .disasm_all(&chunk_item.code, chunk_item.addr)
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
