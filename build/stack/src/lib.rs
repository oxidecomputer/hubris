// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! ## Static Stack Analysis
//!
//! The current static analysis implemented works roughly as follows:
//!
//! 1. We configure LLVM to emit `.stack_sizes` debug information using a `-Z`
//!    flag, which means that the `elf` file compiled for each task contains a
//!    debuginfo section listing the size of stack frames on a per-function
//!    basis.
//! 2. We use the `goblin` crate to parse the compiled `elf` files, and obtain:
//!     1. The per-function stack frame sizes: [`extract_stack_sizes_section()`]
//!     2. The list of all function symbols contained in the elf file,  see
//!        [`fn_symbol_iter()`]
//!     3. The `.text` section of the elf file, which contains executable
//!        instructions, inside of [`extract_function_items()`]
//!     4. Information about where data is inlined within the `.text` section,
//!        see [`extract_text_regions()`]
//! 3. For each function in the elf file, we:
//!     1. Use the [Capstone library](https://www.capstone-engine.org/) (by way
//!        of the [`capstone`](https://docs.rs/capstone/latest/capstone/)
//!        FFI crate) to disassemble the instruction code
//!     2. Iterate through each instruction in the function, and determine all
//!        "outgoing" branches which are calls to other functions
//!     3. Produce a report called [`FunctionData`] for that function which
//!        contains the name, local stack usage, and a list of all called
//!        functions by that function
//! 4. We then start from the entrypoint of the task, `_start`, and recurse
//!    through each node, to calculate the deepest stack usage of any chain of
//!    function calls. See [`get_max_stack()`] and [`Resolver`]'s  `resolve_*`
//!    methods.
//! 5. This "max stack" usage is compared against the `stacksize` for the task
//!    in the manifest, and compilation fails if the calculated max stack
//!    exceeds the written stacksize (outside of this crate).
//!
//! ## Known Limitations
//!
//! This approach already had a couple of known limitations, as they are
//! typical for this approach of static analysis.
//!
//! 1. This approach does not handle recursion, as we have no way to annotate a
//!    potential upper bound of recursive iterations. Currently, the code
//!    counts the number of direct recursion instances detected (e.g. self-calls
//!    of a function), and refuses to resolve call stacks with cycles.
//! 2. This approach does not handle "indirect" branching, which looks something
//!    like `blx r5` in assembly, and is often (but not exclusively) generated
//!    when calling a function through a vtable method, like `dyn Format`.
//!    Currently, the code silently skips considering instances of indirect
//!    branching found.
//! 3. This approach does not handle functions without `.stack_sizes` metadata.
//!    This is commonly present in assembly functions such as
//!    `userlib::sys_send_stub`, or in `compiler-rt`/`llvm` builtins like
//!    `__aeabi_memcpy`. These functions are silently assumed to use zero stack.

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

/// Information derived about a given function
#[derive(Debug)]
pub struct FunctionData {
    pub name: String,
    pub short_name: String,
    pub frame_size: Option<u64>,
    pub calls: BTreeSet<u32>,
    pub missing_calls: usize,
    pub recursive_calls: usize,
}

/// The resulting report of all metadata obtained for functions after
/// extracting from the elf, but before attempting to "resolve" or visit each
/// function to determine max stack usage.
pub struct FunctionReport {
    pub function_items: BTreeMap<u32, FunctionData>,
    pub addr_to_frame_size: BTreeMap<u32, u64>,
    pub names: BTreeMap<u32, String>,
}

/// A function that has been visited and "filled in" by the [`Resolver`].
///
/// Typically stored in an [`Rc`] as each node may appear multiple times across
/// the call graph of a program.
pub struct ResolvedNode {
    pub addr: u32,
    pub name: String,
    pub local_size: Option<u64>,
    pub max_children: u64,
    pub children: BTreeMap<u32, Rc<ResolvedNode>>,
}

/// A tool for turning a [`FunctionReport`] into a hydrated call graph.
///
/// This visits a call graph of [`FunctionData`] recursively, turning each node
/// into a memoized `Rc<ResolvedNode>`, and keeps a mapping of all nodes by
/// address.
pub struct Resolver {
    pub call_stack: Vec<u32>,
    pub all_resolved: BTreeMap<u32, Rc<ResolvedNode>>,
    pub fn_items: BTreeMap<u32, FunctionData>,
}

/// Information derived about a given symbol
struct SymbolData<'a> {
    sym: Sym,
    mangled_name: &'a str,
    base_addr: u64,
    text_region: &'a [u8],
}

/// Information about a "chunk", which is a portion of a function containing
/// executable code (and not inlined data)
struct ChunkItem {
    code: Vec<u8>,
    addr: u64,
}

impl ResolvedNode {
    /// Print the entire stack trace to stdout
    pub fn debug_all(&self) {
        self.debug_all_depth(0, 0);
    }

    /// Print the entire stack trace to stdout, with a given starting depth and
    /// current stack usage at the present depth
    pub fn debug_all_depth(&self, depth: usize, current_stack: u64) {
        let frame_size = self.local_size;
        let stack_depth = current_stack + frame_size.unwrap_or(0);
        for _ in 0..depth {
            print!("  ");
        }
        println!(
            "- 0x{:08X} {} [+{frame_size:?} => {stack_depth}]",
            self.addr, self.name,
        );
        for (_addr, child) in self.children.iter() {
            child.debug_all_depth(
                depth + 1,
                current_stack + frame_size.unwrap_or(0),
            );
        }
    }

    /// Used to determine if `self` could be potentially discarded as a dupe
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

    /// The max stack used by this node, considering this node's stack usage
    /// (if known), and the max stack usage of this node's largest child.
    pub fn max_stack(&self) -> u64 {
        self.local_size.unwrap_or(0) + self.max_children
    }

    /// The worst case call chain starting from this node
    pub fn worst_chain(self: &Rc<Self>) -> Vec<Rc<ResolvedNode>> {
        let mut chain = vec![];
        self.worst_chain_inner(&mut chain);
        chain
    }

    /// used for recursively calculating [`Self::worst_chain_inner`]
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

impl Resolver {
    /// Create a new resolver from the given map of [`FunctionData`] items
    pub fn new(fn_items: BTreeMap<u32, FunctionData>) -> Self {
        Self {
            call_stack: vec![],
            all_resolved: BTreeMap::new(),
            fn_items,
        }
    }

    /// Attempt to resolve a function by name into a [`ResolvedNode`].
    pub fn resolve_by_name(&mut self, entry: &str) -> Result<Rc<ResolvedNode>> {
        let Some(item) = self.fn_items.iter().find(|(_k, v)| &v.name == entry)
        else {
            bail!("Not found");
        };
        let addr = *(item.0);
        self.resolve_addr(addr)
    }

    /// Resolve a function by address into a [`ResolvedNode`].
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
                bail!("Refusing to handle recursion");
            }

            // Resolve the child
            let rchild = self.resolve_addr(child)?;

            let ttl_child = rchild.max_stack();
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

    /// Finds all functions *not* already resolved by the given [`Resolver`] and
    /// attempts to resolve them.
    ///
    /// This finds functions that exist in the elf file, but were not visited
    /// while resolving the call graph, likely due to being called indirectly or
    /// through vtable methods.
    fn find_missing_nodes(
        &mut self,
        all_addrs: impl Iterator<Item = u32>,
    ) -> Vec<Rc<ResolvedNode>> {
        let mut missing = vec![];
        for addr in all_addrs {
            if !self.all_resolved.contains_key(&addr) {
                missing.push(addr);
            }
        }

        let mut found = vec![];
        for addr in missing {
            let node = self.resolve_addr(addr).unwrap();
            found.push(node);
        }

        let mut last_len = found.len();
        loop {
            let mut to_keep = vec![];
            while let Some(val) = found.pop() {
                if found
                    .iter()
                    .any(|f: &Rc<ResolvedNode>| f.is_same_or_child_of(&val))
                    || to_keep
                        .iter()
                        .any(|f: &Rc<ResolvedNode>| f.is_same_or_child_of(&val))
                {
                    continue;
                } else {
                    to_keep.push(val);
                }
            }
            found = to_keep;
            if found.len() == last_len {
                break;
            } else {
                last_len = found.len();
            }
        }

        found
    }
}

impl SymbolData<'_> {
    /// The range of addresses contained within this symbol
    fn addr_range(&self) -> Range<u32> {
        let base_addr = self.base_addr as u32;
        base_addr..base_addr + self.sym.st_size as u32
    }

    /// An array of "Chunks", which are each of the executable portions of this
    /// symbol.
    //
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

////////////////////////////////////////////////////////////////////////////////
// Public methods
////////////////////////////////////////////////////////////////////////////////

/// Load and parse the `elf` file at the given path, producing a report of
/// metadata about all functions in the `elf` file.
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

    let text = build_elf::get_section_by_name(&elf, ".text")
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
        let mut missing_calls = 0;
        let mut recursive_calls = 0;
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
            for (i, (_instr, detail)) in
                instrs.iter().zip(details.iter()).enumerate()
            {
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
                    // Check if this is now not needed anymore
                    if !is_grp_rel(g) && is_tail_call(g) {
                        panic!("this check is NOT obsolete!");
                    }

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
                    //
                    // TODO: We could consider keeping track of register ops
                    // and potentially figure out the location of the register
                    // based jump here
                    let arm::ArmOperandType::Imm(target) = op.op_type else {
                        if verbose {
                            println!(
                                "Failed to resolve indirect call in {name}!"
                            );
                        }
                        // TODO: we record that we are missing a call, and
                        // ideally we would plug the worst of the missing funcs
                        // here. Unfortunately, that means we end up needing to
                        // invalidate the RC, which would be disappointing. We
                        // could potentially add an AtomicU64 here to add
                        // "bonus" items, but that would again invalidate the
                        // pre-computed "max stack" numbers.
                        missing_calls += 1;
                        continue;
                    };
                    let target = u32::try_from(target).unwrap();

                    // Avoid recursive calls to the same function, and ignore
                    // any control flow that directs back inside this function.
                    // Any other jumps should be recorded as a call.
                    if target == base_addr {
                        recursive_calls += 1;
                    } else if !function_range.contains(&target) {
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
                missing_calls,
                recursive_calls,
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
    let mut resolver = Resolver::new(fns.function_items);
    let node = resolver.resolve_by_name("_start")?;
    let chain = node.worst_chain();
    let mut chain_compat = chain
        .into_iter()
        .map(|n| (n.local_size.unwrap_or(0), n.name.clone()))
        .collect::<Vec<_>>();

    let missing =
        resolver.find_missing_nodes(fns.addr_to_frame_size.keys().copied());

    // Find the largest missing item so that we can add it to the call stack as
    // a "fudge factor" for unresolved dynamic/indirect dispatch.
    let largest = missing.iter().map(|n| n.max_stack()).max().unwrap_or(0);
    if let Some(node) = missing.iter().find(|n| n.max_stack() == largest) {
        chain_compat.push((largest, format!("missing::{}", node.name)));
    }

    Ok(chain_compat)
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
    let sizes = build_elf::get_section_by_name(parsed_elf, ".stack_sizes")
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

/// There are `$t` and `$d` symbols which indicate the beginning of text
/// versus data in the `.text` region.  We collect them into a `BTreeMap`
/// here so that we can avoid trying to decode inline data words.
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

/// Returns an iterator over symbols inside the elf file that represent a named
/// function.
fn fn_symbol_iter<'a>(
    parsed_elf: &Elf<'a>,
    text_section: &SectionHeader,
    raw_elf: &'a [u8],
) -> impl Iterator<Item = SymbolData<'a>> {
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
            Some(SymbolData {
                sym: s,
                // TODO: assert?
                mangled_name: parsed_elf.strtab.get_at(s.st_name).unwrap(),
                base_addr,
                text_region,
            })
        })
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
