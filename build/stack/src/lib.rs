// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    path::Path,
    rc::Rc,
};

use anyhow::{Context, Result, anyhow, bail};
use capstone::{
    Capstone, Insn, InsnDetail, InsnGroupId, InsnGroupType,
    arch::{
        ArchDetail, BuildsCapstone, BuildsCapstoneExtraMode, DetailsArchInsn,
        arm,
    },
};
use goblin::elf::{Elf, SectionHeader, Sym};

// We'll be packing everything into this data structure
#[derive(Debug)]
pub struct FunctionData {
    pub name: String,
    pub short_name: String,
    pub frame_size: Option<u64>,
    pub calls: BTreeSet<u32>,
}

struct SymbolItem<'a> {
    sym: Sym,
    mangled_name: &'a str,
    base_addr: u64,
    text_region: &'a [u8],
}

struct ChunkItem {
    code: Vec<u8>,
    addr: u64,
}

pub struct FunctionReport {
    pub function_items: BTreeMap<u32, FunctionData>,
    pub addr_to_frame_size: BTreeMap<u32, u64>,
    pub names: BTreeMap<u32, String>,
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
    let relevant = parsed_elf
        .syms
        .iter()
        .filter(|s| s.st_name != 0)
        .filter(|s| s.st_size == 0)
        .filter(|s| s.st_type() == goblin::elf::sym::STT_NOTYPE);

    for sym in relevant {
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

impl SymbolItem<'_> {
    fn addr_range(&self) -> Range<u32> {
        let base_addr = self.base_addr as u32;
        base_addr..base_addr + self.sym.st_size as u32
    }

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
                // TODO: assert?
                mangled_name: parsed_elf.strtab.get_at(s.st_name).unwrap(),
                base_addr,
                text_region,
            })
        })
}

pub fn extract_function_items(
    elf: &Path,
    task_name: &str,
    verbose: bool,
) -> Result<FunctionReport> {
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
    let mut fn_names = BTreeMap::new();
    for sym_item in fn_symbol_iter(&elf, text, &data) {
        let base_addr = sym_item.base_addr as u32;
        let name = sym_item.mangled_name;
        // This is the stack frame size of the current function
        let frame_size = addr_to_frame_size.get(&base_addr).copied();
        // This is the range of addresses comprising this function
        let function_range = sym_item.addr_range();
        // Demangled and short name of this function
        let name = rustc_demangle::demangle(name).to_string();
        let short_name = name_shortener(&name);

        fn_names.insert(base_addr, name.clone());

        // Split the text region into instruction-only chunks
        let chunks = sym_item.extract_instruction_chunks(is_code);

        // Walk through each "chunk", which is an island of executable code
        // inside of each function, and collect all the out-bound calls. We
        // disassemble chunks rather than functions, as functions might contain
        // puddles of inline data which we don't want to (mis)-disassemble.
        let mut calls = BTreeSet::new();
        for chunk_item in chunks {
            let instrs = cs
                .disasm_all(&chunk_item.code, chunk_item.addr)
                .map_err(|e| anyhow!("disassembly failed: {e:?}"))?;

            // We need to get details for the instruction, which we should
            // always have for well-formed programs
            let instrs: Vec<&Insn<'_>> = instrs.iter().collect();
            let details: Vec<InsnDetail<'_>> = instrs
                .iter()
                .map(|instr| {
                    cs.insn_detail(instr).map_err(|e| {
                        anyhow!("could not get instruction details: {e}")
                    })
                })
                .collect::<Result<_>>()?;

            // Walk through each instruction inside of each chunk
            for (i, detail) in details.iter().enumerate() {
                let can_tail = frame_size == Some(0) && i == instrs.len() - 1;
                let is_grp_call =
                    |g| g == &InsnGroupId(InsnGroupType::CS_GRP_CALL as u8);
                let is_grp_jump =
                    |g| g == &InsnGroupId(InsnGroupType::CS_GRP_JUMP as u8);
                let is_grp_rel = |g| {
                    g == &InsnGroupId(
                        InsnGroupType::CS_GRP_BRANCH_RELATIVE as u8,
                    )
                };

                // Detect tail calls, which are jumps at the final instruction
                // when the function itself has no stack frame.
                let is_tail_call = |g| is_grp_jump(g) && can_tail;

                let is_branching_instr = detail.groups().iter().any(|g| {
                    is_grp_call(g) || is_grp_rel(g) || is_tail_call(g)
                });

                if is_branching_instr {
                    // On Arm/Thumb, a jump always has some kind of operand,
                    // which is where we are jumping to. Get the last operand so
                    // we can determine if we can follow this.
                    let arch = detail.arch_detail();
                    let ArchDetail::ArmDetail(details) = arch else {
                        panic!("Unsupported arch");
                    };
                    let op = details.operands().last().unwrap_or_else(|| {
                        panic!("missing operand!");
                    });
                    // We can't resolve indirect calls, alas
                    let arm::ArmOperandType::Imm(target) = op.op_type else {
                        if verbose {
                            println!(
                                "Failed to resolve indirect call in {name}!"
                            );
                        }
                        continue;
                    };
                    let target = u32::try_from(target).unwrap();

                    // Avoid recursive calls into the same function (or midway
                    // into the function, which is a thing we've seen before!
                    // it's weird!)
                    if !function_range.contains(&target) {
                        calls.insert(target);
                    }
                }
            }
        }

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

    Ok(FunctionReport {
        function_items: fns,
        addr_to_frame_size,
        names: fn_names,
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
    let fns = extract_function_items(elf, task_name, verbose)?;
    get_max_stack_inner(fns, task_name, verbose)
}

pub fn get_max_stack_inner(
    fns: FunctionReport,
    task_name: &str,
    verbose: bool,
) -> Result<Vec<(u64, String)>> {
    let fns = fns.function_items;
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

fn name_shortener(name: &str) -> String {
    // Strip the trailing hash from the name for ease of printing
    if let Some(i) = name.rfind("::") {
        &name[..i]
    } else {
        &name
    }
    .to_owned()
}

pub struct ResolvedNode {
    pub addr: u32,
    pub name: String,
    pub local_size: Option<u64>,
    pub max_children: u64,
    pub children: BTreeMap<u32, Rc<ResolvedNode>>,
}

impl ResolvedNode {
    pub fn debug_all(&self) {
        self.debug_all_depth(0, 0);
    }

    pub fn debug_all_depth(&self, depth: usize, current_stack: u64) {
        let frame_size = self.local_size.unwrap_or(0);
        let stack_depth = current_stack + frame_size;
        for _ in 0..depth {
            print!("  ");
        }
        println!(
            "- 0x{:08X} {} [+{frame_size} => {stack_depth}]",
            self.addr, self.name,
        );
        for (_addr, child) in self.children.iter() {
            child.debug_all_depth(depth + 1, current_stack + frame_size);
        }
    }

    /// Used to determine if `other` should be discarded
    pub fn is_same_or_child_of(&self, other: &Self) -> bool {
        if other.addr == self.addr {
            return true;
        }
        for (_caddr, child) in other.children.iter() {
            if self.is_same_or_child_of(child) {
                return true;
            }
        }
        false
    }

    pub fn max_stack(&self) -> u64 {
        self.local_size.unwrap_or(0) + self.max_children
    }

    pub fn worst_chain(self: &Rc<Self>) -> Vec<Rc<ResolvedNode>> {
        let mut chain = vec![];
        self.worst_chain_inner(&mut chain);
        chain
    }

    fn worst_chain_inner(self: &Rc<Self>, chain: &mut Vec<Rc<ResolvedNode>>) {
        chain.push(self.clone());
        if let Some(child) = self
            .children
            .iter()
            .find(|c| c.1.max_stack() == self.max_children)
        {
            child.1.worst_chain_inner(chain)
        }
    }
}

pub struct Resolver {
    pub call_stack: Vec<u32>,
    pub all_resolved: BTreeMap<u32, Rc<ResolvedNode>>,
    pub fn_items: BTreeMap<u32, FunctionData>,
}

impl Resolver {
    pub fn new(fn_items: BTreeMap<u32, FunctionData>) -> Self {
        Self {
            call_stack: vec![],
            all_resolved: BTreeMap::new(),
            fn_items,
        }
    }

    pub fn resolve_by_name(&mut self, entry: &str) -> Result<Rc<ResolvedNode>> {
        let Some(item) = self.fn_items.iter().find(|(_k, v)| &v.name == entry)
        else {
            bail!("Not found");
        };
        let addr = *(item.0);
        self.resolve_addr(addr)
    }

    pub fn resolve_addr(&mut self, addr: u32) -> Result<Rc<ResolvedNode>> {
        // Have we already resolved this node?
        if let Some(node) = self.all_resolved.get(&addr) {
            return Ok(node.clone());
        }

        // no, we havent. Get the node info from the fn data
        let Some(item) = self.fn_items.get(&addr) else {
            bail!("no function data");
        };
        self.call_stack.push(addr);
        let children = item.calls.clone();
        let name = item.name.clone();
        let local_size = item.frame_size;

        let mut res_children = BTreeMap::new();
        let mut max_children = 0;
        for child in children {
            if self.call_stack.contains(&child) {
                bail!("Refusing to handle recursive");
            }

            // Resolve the child
            let rchild = self.resolve_addr(child)?;

            let ttl_child =
                rchild.local_size.unwrap_or(0) + rchild.max_children;
            max_children = max_children.max(ttl_child);
            res_children.insert(child, rchild);
        }

        let new_node = Rc::new(ResolvedNode {
            addr,
            name,
            local_size,
            max_children,
            children: res_children,
        });
        self.all_resolved.insert(addr, new_node.clone());
        self.call_stack.pop();
        Ok(new_node)
    }
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
