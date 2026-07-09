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
//!     2. "Raw" function information per-function, see
//!        [`extract_raw_function_data`], containing:
//!         1. The list of all function symbols contained in the elf file
//!         2. The `.text` section of the elf file, which contains executable
//!            instructions
//!         3. Information about where data is inlined within the `.text`
//!            section
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
    Capstone, InsnDetail, InsnGroupId, InsnGroupType,
    arch::{
        ArchDetail, BuildsCapstone, BuildsCapstoneExtraMode, DetailsArchInsn,
        arm,
    },
};
use goblin::elf::{Elf, SectionHeader, Sym};

pub const KNOWN_RECURSORS: &[&str] = &[
    // slice_error_fail calls slice_error_fail_rt which calls slice_error_fail
    "str::slice_error_fail",
    // `smoltcp::iface::interface::InterfaceInner::dispatch_ip::<VLanTxToken>`
    // self-recurses. This fragment is a little weird because it looks like:
    //
    // ```
    // <smoltcp[b3fa0c29b1d616a8]::iface::interface::InterfaceInner>
    //   ::dispatch_ip::<task_net[f938ea0ef13e6f35]::server_impl::VLanTxToken>
    // ```
    "dispatch_ip",
    // We have limited recursion here, see
    // https://github.com/oxidecomputer/hubris/issues/2593
    "Vsc7448Spi",
];

pub const KNOWN_TO_IGNORE: &[&str] = &["stackblow"];

/// Configuration for public methods
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
    /// Function names containing any of these patterns will be allowed to
    /// recurse
    pub allowed_recurses: Vec<String>,

    /// Functions names containing any of these patterns will be totally ignored
    pub ignored_functions: Vec<String>,
}

/// Information derived about a given function
#[derive(Debug, Clone)]
pub struct FunctionData {
    pub name: String,
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
    pub config: Config,
}

/// Describes where a symbol lives.
#[derive(Debug)]
enum SymSection<'a> {
    /// Defined in a real section; carries the section name.
    Section(&'a str),
    /// Undefined (imported / external) — st_shndx == SHN_UNDEF.
    Undefined,
    /// Absolute value, not tied to a section — SHN_ABS.
    Absolute,
    /// Common block (tentative definition) — SHN_COMMON.
    Common,
    /// Real index lives in a SHT_SYMTAB_SHNDX section — SHN_XINDEX.
    Extended,
}

/// "Raw" function data, extracted from the elf metadata and section headers
#[derive(Debug)]
struct RawFunctionData {
    /// Demangled name of the function
    name: String,
    /// Address of the function
    base_addr: u32,
    /// The size of the function, in bytes, according to the symbol table
    symbol_table_size: Option<u64>,
    /// The calculated size of the function, counting until the next symbol
    /// starts (useful when the symbol table is missing size)
    calc_size: u64,
    /// The address ranges that contain executable code (and not inline data)
    text_ranges: Vec<TextRange>,
}

/// A range of executable "Text" data
#[derive(Debug)]
enum TextRange {
    /// An open-ended range starting at the given address
    TextStart { start: u32 },
    /// A closed-ended range `start..end`.
    TextRange { start: u32, end: u32 },
}

/// Private helper type to group up all of the context information we need when
/// processing the raw assembly of each function to figure out which functions
/// are called by this function
struct FunctionCallCollector<'a> {
    /// The Capstone disassembler
    cs: &'a Capstone,
    /// The name of this function
    name: &'a str,
    /// The base address of this function
    base_addr: u32,
    /// The range of addresses on the target that contain this function, as well
    /// as any data sections within/after this function
    function_range: Range<u32>,
    /// The stack frame size for this function, as reported by LLVM's
    /// `.stack_sizes` section
    frame_size: Option<u64>,
    /// Whether to print debugging info
    verbose: bool,
    /// The number of outgoing calls we were unable to resolve, likely because
    /// of indirect addressing via function pointers/dyn Trait
    missing_calls: usize,
    /// The number of self-recursive calls, e.g. this function directly calling
    /// itself
    recursive_calls: usize,
    /// All outgoing calls from this function
    calls: BTreeSet<u32>,
}

////////////////////////////////////////////////////////////////////////////////
// Public methods
////////////////////////////////////////////////////////////////////////////////

/// Estimates the maximum stack size for the given task
///
/// This does not take dynamic function calls into account, which could cause
/// underestimation.  Overestimation is less likely, but still may happen if
/// there are logically impossible call trees (e.g. `A -> B` and `B -> C`, but
/// `B` never calls `C` if called by `A`).
pub fn get_max_stack(
    config: Config,
    elf: &Path,
    verbose: bool,
) -> Result<Vec<(u64, String)>> {
    let fns = extract_function_items(elf, verbose, &config)?;
    let mut resolver = Resolver::new(fns.function_items, &config);
    let node = resolver.resolve_by_name("_start")?;
    let chain = node.worst_chain();
    let mut chain_compat = chain
        .into_iter()
        .map(|n| (n.local_size.unwrap_or(0), n.name.clone()))
        .collect::<Vec<_>>();

    let missing = resolver.find_missing_nodes()?;

    // Find the largest missing item so that we can add it to the call stack as
    // a "fudge factor" for unresolved dynamic/indirect dispatch.
    let largest = missing.iter().map(|n| n.max_stack()).max().unwrap_or(0);
    if let Some(node) = missing.iter().find(|n| n.max_stack() == largest) {
        chain_compat.push((largest, format!("missing::{}", node.name)));
    }

    Ok(chain_compat)
}

/// Load and parse the `elf` file at the given path, producing a report of
/// metadata about all functions in the `elf` file.
pub fn extract_function_items(
    elf: &Path,
    verbose: bool,
    config: &Config,
) -> Result<FunctionReport> {
    // Open the statically-linked ELF file
    let data = std::fs::read(elf).with_context(|| {
        format!("could not open ELF file: {}", elf.display())
    })?;
    let elf = goblin::elf::Elf::parse(&data)
        .with_context(|| format!("could not parse {}", elf.display()))?;

    // Get sizes of stack frames by addr from the elf
    let addr_to_frame_size = extract_stack_sizes_section(&data, &elf)?;

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

    let functions = extract_raw_function_data(&elf, text)?;

    for (base_addr, function) in functions {
        // This is the stack frame size of the current function
        let frame_size = addr_to_frame_size.get(&base_addr).copied();
        let function_range = function.function_range();

        // Gather up all the information we need to find all the calls in this
        // function
        let mut fc = FunctionCallCollector {
            cs: &cs,
            name: &function.name,
            base_addr,
            function_range,
            frame_size,
            verbose,
            missing_calls: 0,
            recursive_calls: 0,
            calls: BTreeSet::new(),
        };

        // Walk through each "text range", which is an island of executable code
        // inside of each function, and collect all the out-bound calls. We
        // disassemble ranges rather than functions, as functions might contain
        // puddles of inline data which we don't want to (mis)-disassemble.
        for chunk in function.text_ranges.iter() {
            let (data_addr, data) = chunk.text_data(&data, text)?;
            fc.extract_calls(data, data_addr)?;
        }

        if fc.recursive_calls != 0 {
            let allow_match = config.match_allowed_recurses(&fc.name);

            if let Some(am) = allow_match {
                // For now, pragmatically, we'll just ignore this recursive
                // call site.
                println!(
                    "WARN: Allowing {} to self-recurse, matching {}",
                    fc.name, am
                );
            } else {
                bail!("Refusing to handle self-recursion of {}", fc.name);
            }
        }

        fns.insert(
            base_addr,
            FunctionData {
                calls: fc.calls,
                missing_calls: fc.missing_calls,
                recursive_calls: fc.recursive_calls,
                name: function.name,
                frame_size,
            },
        );
    }

    Ok(FunctionReport {
        function_items: fns,
        addr_to_frame_size,
    })
}

////////////////////////////////////////////////////////////////////////////////
// impls
////////////////////////////////////////////////////////////////////////////////

impl Default for Config {
    fn default() -> Self {
        Self {
            allowed_recurses: KNOWN_RECURSORS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ignored_functions: KNOWN_TO_IGNORE
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

impl Config {
    fn match_allowed_recurses(&self, name: &str) -> Option<&str> {
        self.allowed_recurses
            .iter()
            .find(|am| name.contains(am.as_str()))
            .map(String::as_str)
    }

    fn match_ignored_functions(&self, name: &str) -> Option<&str> {
        self.ignored_functions
            .iter()
            .find(|am| name.contains(am.as_str()))
            .map(String::as_str)
    }
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
    pub fn new(fn_items: BTreeMap<u32, FunctionData>, config: &Config) -> Self {
        Self {
            call_stack: vec![],
            all_resolved: BTreeMap::new(),
            fn_items,
            config: config.clone(),
        }
    }

    /// Attempt to resolve a function by name into a [`ResolvedNode`].
    pub fn resolve_by_name(&mut self, entry: &str) -> Result<Rc<ResolvedNode>> {
        let Some(item) = self.fn_items.iter().find(|(_k, v)| v.name == entry)
        else {
            bail!("function '{entry}' not found");
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
            bail!("no function data for {addr:08X}");
        };
        self.call_stack.push(addr);
        let children = item.calls.clone();
        let name = item.name.clone();
        let local_size = item.frame_size;

        let mut res_children = BTreeMap::new();
        let mut max_children = 0;
        for child in children {
            // Is this a recursive callsite?
            if self.call_stack.contains(&child) {
                let Some(child_info) = self.fn_items.get(&child) else {
                    bail!("{child:08X}: no child function data");
                };
                let allow_match =
                    self.config.match_allowed_recurses(&child_info.name);

                if let Some(am) = allow_match {
                    // For now, pragmatically, we'll just ignore this recursive
                    // call site.
                    println!(
                        "WARN: Allowing {} to recurse, matching {}",
                        child_info.name, am
                    );
                    continue;
                }

                bail!(
                    "Refusing to handle recursion of {}, {:08X?} + {:08X}",
                    child_info.name,
                    self.call_stack,
                    child
                );
            }

            // Resolve the child
            let rchild = self
                .resolve_addr(child)
                .with_context(|| format!("While resolving {}", name))?;

            // Is this an ignored function?
            let ignore_match =
                self.config.match_ignored_functions(&rchild.name);
            if let Some(ignore) = ignore_match {
                println!("WARN: Ignoring {}, matching {}", rchild.name, ignore);
                continue;
            }

            // Keep it!
            max_children = max_children.max(rchild.max_stack());
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
    fn find_missing_nodes(&mut self) -> Result<Vec<Rc<ResolvedNode>>> {
        // Find all of the functions we know about in `fn_items`, and find
        // any that haven't already been resolved into `all_resolved` from
        // previous resolution, usually the `_start` entry point
        let mut found = self
            .fn_items
            .keys()
            .copied()
            .filter(|addr| !self.all_resolved.contains_key(addr))
            .collect::<Vec<_>>() // necessary to avoid double borrowing self
            .into_iter()
            .map(|addr| self.resolve_addr(addr))
            .collect::<Result<Vec<_>, _>>()?;

        // Filter out any ignored functions
        found.retain(|rn| {
            self.config.match_ignored_functions(&rn.name).is_none()
        });

        Ok(found)
    }
}

/// Gets the linking section that the given symbol is a part of.
fn section_of<'a>(elf: &'a Elf<'_>, sym: &Sym) -> SymSection<'a> {
    match sym.st_shndx as u32 {
        goblin::elf::section_header::SHN_UNDEF => SymSection::Undefined,
        goblin::elf::section_header::SHN_ABS => SymSection::Absolute,
        goblin::elf::section_header::SHN_COMMON => SymSection::Common,
        goblin::elf::section_header::SHN_XINDEX => SymSection::Extended,
        idx => {
            match elf.section_headers.get(idx as usize) {
                Some(shdr) => elf
                    .shdr_strtab
                    .get_at(shdr.sh_name)
                    .map(SymSection::Section)
                    .unwrap_or(SymSection::Undefined),
                None => SymSection::Undefined, // out-of-range / malformed
            }
        }
    }
}

impl RawFunctionData {
    /// Obtain "Raw" function data for a given [`Sym`].
    ///
    /// This starts with no text ranges yet, and requires calls to
    /// `push_text_*`.
    pub fn new(entry: Sym, parsed_elf: &Elf<'_>, base_addr: u32) -> Self {
        // Get the name, if there is one in the symbol table, otherwise
        // make one up.
        let name = if entry.st_name == 0 {
            None
        } else {
            parsed_elf
                .strtab
                .get_at(entry.st_name)
                .map(|n| rustc_demangle::demangle(n).to_string())
        };
        let name = name.unwrap_or_else(|| format!("anon_fn_0x{base_addr:08X}"));

        // Get the size, if there is one
        let size = if entry.st_size == 0 {
            None
        } else {
            Some(entry.st_size)
        };

        RawFunctionData {
            name,
            base_addr,
            symbol_table_size: size,
            text_ranges: vec![],

            // For now, we don't calculate the size
            calc_size: 0,
        }
    }

    /// Note where a text section ends
    pub fn push_text_end(&mut self, mut addr: u32) -> Result<()> {
        // We may push the text end when we reach the end of the function. If
        // there is padding, this may be a garbage D4D4 instruction inserted by
        // the linker!
        if let Some(size) = self.symbol_table_size.as_ref() {
            addr = addr.min(self.base_addr + (*size as u32));
        }

        match self.text_ranges.last_mut() {
            None => {
                bail!("Unexpected data/end of fn before text at {addr:08X}")
            }
            Some(val) => match val {
                TextRange::TextStart { start } => {
                    let start = *start;
                    *val = TextRange::TextRange { start, end: addr };
                }
                TextRange::TextRange { .. } => {
                    // This could happen if we had text, then data, then the
                    // next function, because we call `push_text_end` both when
                    // we see a `$d` record or when the function ends.
                }
            },
        }
        Ok(())
    }

    /// Not where a text section starts
    pub fn push_text_start(&mut self, addr: u32) -> Result<()> {
        if let Some(TextRange::TextStart { start: old }) =
            self.text_ranges.last()
        {
            bail!("Back to back Text Starts! {old:08X} => {addr:08X}");
        }
        self.text_ranges.push(TextRange::TextStart { start: addr });
        Ok(())
    }

    /// Get the range of addresses contained by this function. This does
    /// include any inline data.
    pub fn function_range(&self) -> Range<u32> {
        let start = self.base_addr;
        let size = if let Some(addr) = self.symbol_table_size.as_ref() {
            *addr
        } else {
            self.calc_size
        } as u32;
        start..(start + size)
    }
}

impl TextRange {
    /// Extract the raw bytes of assembly for this text range
    fn text_data<'a>(
        &self,
        raw_elf: &'a [u8],
        text_section: &SectionHeader,
    ) -> Result<(u64, &'a [u8])> {
        let (start, end) = match self {
            TextRange::TextStart { start } => {
                bail!("Missing text end: {start:08X}")
            }
            TextRange::TextRange { start, end } => (*start, *end),
        };
        // Get the text region for this function
        let start_offset = (start as u64 - text_section.sh_addr
            + text_section.sh_offset) as usize;
        let end_offset = (end as u64 - text_section.sh_addr
            + text_section.sh_offset) as usize;
        Ok((start as u64, &raw_elf[start_offset..end_offset]))
    }
}

impl FunctionCallCollector<'_> {
    // attempt to extract all outgoing calls from the given chunk of executable
    // code within this function
    fn extract_calls(&mut self, data: &[u8], data_addr: u64) -> Result<()> {
        let instrs = self
            .cs
            .disasm_all(data, data_addr)
            .map_err(|e| anyhow!("disassembly failed: {e:?}"))?;

        let Some((last, all_except_last)) = instrs.split_last() else {
            bail!("Empty chunk in {} at {:08X}", self.name, data_addr);
        };

        // See https://www.nmichaels.org/musings/d4d4/d4d4/
        let to_consider = if last.bytes() == [0xD4, 0xD4] {
            all_except_last
        } else {
            &instrs
        };

        // Walk through each instruction inside of each chunk
        for (i, instr) in to_consider.iter().enumerate() {
            // We need to get details for the instruction, which we should
            // always have for well-formed programs
            let detail = self.cs.insn_detail(instr).map_err(|e| {
                anyhow!("could not get instruction details: {e}")
            })?;
            self.process_one_instruction(
                &detail,
                i == (to_consider.len() - 1),
            )?;
        }
        Ok(())
    }

    /// Process a single instruction, and determine if it is making an outgoing
    /// call
    fn process_one_instruction(
        &mut self,
        detail: &InsnDetail<'_>,
        last_in_chunk: bool,
    ) -> Result<()> {
        let can_tail = self.frame_size == Some(0) && last_in_chunk;
        let is_grp_call =
            |g| g == &InsnGroupId(InsnGroupType::CS_GRP_CALL as u8);
        let is_grp_jump =
            |g| g == &InsnGroupId(InsnGroupType::CS_GRP_JUMP as u8);
        let is_grp_rel =
            |g| g == &InsnGroupId(InsnGroupType::CS_GRP_BRANCH_RELATIVE as u8);

        // Detect tail calls, which are jumps at the final instruction
        // when the function itself has no stack frame.
        let is_tail_call = |g| is_grp_jump(g) && can_tail;
        let is_branching_instr = detail.groups().iter().any(|g| {
            // NOTE: `is_tail_call` was a check before we started checking
            // `is_grp_rel`. As of 2026-07-09, James *thinks* the latter check
            // is a superset of the former, and we can probably remove it, but
            // isn't that sure about it, and hasn't had time to look deeper.
            // For now, we leave this as a tombstone to see if we ever observe
            // this in practice.
            if !is_grp_rel(g) && is_tail_call(g) {
                panic!(
                    "this check is NOT obsolete, please @jamesmunns about this"
                );
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
                panic!("jumps on ARM should always have an operand!");
            });
            // We can't resolve indirect calls, alas
            //
            // TODO: We could consider keeping track of register ops
            // and potentially figure out the location of the register
            // based jump here
            let arm::ArmOperandType::Imm(target) = op.op_type else {
                if self.verbose {
                    println!(
                        "Failed to resolve indirect call in {}!",
                        self.name
                    );
                }
                // TODO: we record that we are missing a call, and
                // ideally we would plug the worst of the missing funcs
                // here. Unfortunately, that means we end up needing to
                // invalidate the RC, which would be disappointing. We
                // could potentially add an AtomicU64 here to add
                // "bonus" items, but that would again invalidate the
                // pre-computed "max stack" numbers.
                self.missing_calls += 1;
                return Ok(());
            };
            let target = u32::try_from(target).unwrap();

            // Note any recursive calls to the same function, and ignore
            // any control flow that directs back inside this function.
            // Any other jumps should be recorded as a call.
            //
            // We'll check this at the end if there were any instances
            // of self-recursion
            if target == self.base_addr {
                self.recursive_calls += 1;
            } else if !self.function_range.contains(&target) {
                self.calls.insert(target);
            }
        }
        Ok(())
    }
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

/// Extract "raw" function data, which contains information local to each
/// function, without yet considering call graph data.
fn extract_raw_function_data(
    parsed_elf: &Elf<'_>,
    text_section: &SectionHeader,
) -> Result<BTreeMap<u32, RawFunctionData>> {
    // We're going to walk through each of the symbols in the symbol table,
    // pulling any item that is part of the `.text` or executable section.
    // This section may include multiple entries for the same address, we'll
    // explain that more shortly! But for now, "bunch" them into buckets of
    // symbols at the same address. We're *also* doing this because the symbols
    // may not necessarily be in-order in the symbol table.
    let mut buckets = BTreeMap::<u32, Vec<Sym>>::new();
    for sym in parsed_elf.syms.iter() {
        // If it's not in the `.text` table, we're not interested!
        let section = section_of(parsed_elf, &sym);
        let SymSection::Section(".text") = section else {
            continue;
        };

        // Mask the lowest bit off, this is because thumb functions are listed
        // with this bit set, because that controls the "mode" on jump in arm
        // processors.
        let base_addr = sym.st_value & !1;

        // Get the existing bucket for this address, or make a new bucket.
        let syms = buckets.entry(base_addr as u32).or_default();
        syms.push(sym);
    }

    // Great, now we have a bunch of buckets! We expect to see something that
    // looks like this, if you were to look at it with `objdump`:
    //
    // ```text
    // 00012078 l       .text	00000000 $t
    // 00012078 l     F .text	00000064 func_one
    // 000120d0 l       .text	00000000 $d
    // 000120dc l       .text	00000000 $t
    // 000120dc l     F .text	00000014 func_two
    // ```
    //
    // This is a bit harder to read, because it's out of order, as mentioned
    // above, but we would expect three buckets in our code now:
    //
    // * 00012078 with two items ($t and func_one)
    // * 000120d0 with one item ($d)
    // * 000120dc with two items ($t and func_two)
    //
    // This might make more sense if we arrange this correctly:
    //
    // ```text
    // 00012078 l     F .text	00000064 func_one
    // 00012078 l       .text	00000000 $t
    // 000120d0 l       .text	00000000 $d
    // 000120dc l     F .text	00000014 func_two
    // 000120dc l       .text	00000000 $t
    // ```
    //
    // What we actually have here is two functions, `func_one` at 00012078, and
    // `func_two` at `000120dc`. `func_one` has three symbols: The one marked
    // `F`, which is the actual function symbol, showing it has a length of 64,
    // and the name "func_one". Then it has `$t` at the same address, which
    // shows that it starts with "executable code" first. Later, it has a `$d`
    // symbol, which notes that inline data is here at the end of the function.
    // After this, `func_two` begins.
    //
    // The following code is a small state machine that tries to extract all the
    // symbols related to a single function into an aggregated "ElfFunctionData"
    // report, which we will later use.
    let mut functions = BTreeMap::<u32, RawFunctionData>::new();
    let mut current: Option<RawFunctionData> = None;

    // For each bucket...
    for (base_addr, mut syms) in buckets {
        // ... while there is still any contents in this bucket...
        while !syms.is_empty() {
            // ..see if there is a function entry still in this bucket. If there
            // is, remove it from the bucket and process it.
            if let Some(entry) = {
                syms.iter()
                    .position(|s| s.is_function())
                    .map(|idx| syms.remove(idx))
            } {
                // Found one, if there was a pending ElfFunctionData, "commit"
                // it, using the current address to calculate the size, in case
                // the function item was missing "function size" metadata, which
                // we have observed with some externally linked static
                // libraries, like `salty`.
                if let Some(mut ch) = current.take() {
                    let calc_size = (base_addr - ch.base_addr) as u64;
                    ch.calc_size = calc_size;
                    ch.push_text_end(base_addr)?;
                    functions.insert(ch.base_addr, ch);
                }

                // Store off the current chunk
                current =
                    Some(RawFunctionData::new(entry, parsed_elf, base_addr));
                continue;
            }

            // We know `syms` is non-empty, take some item in it
            let entry = syms.pop().unwrap();

            // Nope, see if we can extract $d/$t symbols to the current fn.
            // We *should* always have a function entry by now, but if not,
            // just log a warning.
            let Some(current) = current.as_mut() else {
                bail!("Discarded {entry:?} at {base_addr:08X}");
            };

            match parsed_elf.strtab.get_at(entry.st_name) {
                Some("$t") => current.push_text_start(base_addr)?,
                Some("$d") => current.push_text_end(base_addr)?,
                _other => {
                    // Ignore other symbols, like `_stext`, etc.
                }
            }
        }
    }

    // Finally push the last chunk onto the list
    if let Some(mut cur) = current.take() {
        // TODO! We need to figure out the end of the section here to fill in
        // calculated length!
        let text_end = text_section.sh_addr + text_section.sh_size;
        cur.calc_size = text_end - cur.base_addr as u64;
        cur.push_text_end(text_end as u32)?;
        functions.insert(cur.base_addr, cur);
    }

    Ok(functions)
}
