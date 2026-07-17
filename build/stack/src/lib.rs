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
//! 4. We then build a directed graph of all function calls, condense any
//!    recursive cycles into single nodes (refusing to continue unless the
//!    cycle is allow-listed, see [`Config::allowed_recurses`]), and calculate
//!    the deepest stack usage of any chain of function calls with a single
//!    pass over the graph in dependency (topological) order, starting from
//!    the entrypoint of the task, `_start`. See [`get_max_stack()`] and
//!    [`Resolver`]'s `resolve_*` methods.
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
//!    of a function), and refuses to resolve call stacks with cycles, UNLESS
//!    a member of the cycle is included on the "allowed_recurses" list or
//!    "ignored_functions" list, in which case these items are skipped and not
//!    counted.
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
use petgraph::{
    Direction,
    algo::{condensation, toposort},
    graph::{Graph, NodeIndex},
    graphmap::DiGraphMap,
    visit::Dfs,
};

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

pub const KNOWN_TO_IGNORE: &[&str] = &[
    // Our on-target test framework has an explicit "stackblow" function that
    // intentionally causes a stack overflow. This unsurprisingly makes the
    // max stack analysis upset.
    "stackblow",
];

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
#[derive(Debug)]
pub struct ResolvedNode {
    pub addr: NodeAddrKind,
    pub name: String,
    pub local_size: Option<u64>,
    pub max_children: u64,
    pub children: Vec<Rc<ResolvedNode>>,
}

/// A tool for turning a [`FunctionReport`] into a hydrated call graph.
///
/// Internally, this builds a [`petgraph`] directed graph of all functions,
/// condenses each strongly connected component (i.e. each recursive cycle)
/// into a single node, and then computes the max stack usage of every
/// function with one pass over the resulting graph in reverse topological
/// order, turning each node into a memoized `Rc<ResolvedNode>`.
///
/// Because the condensed graph is guaranteed to be acyclic, every node's
/// max stack is a context-free property of the node itself, so the results
/// are independent of the order in which functions are resolved.
pub struct Resolver {
    pub all_resolved: BTreeMap<u32, Rc<ResolvedNode>>,
    pub fn_items: BTreeMap<u32, FunctionData>,
    pub config: Config,
    /// The condensed (cycle-free) call graph. Each node is one strongly
    /// connected component of the function call graph, carrying the base
    /// addresses of its member functions. `None` until first resolution.
    condensed: Option<Graph<Vec<u32>, ()>>,
    /// Maps each function address to its node in [`Self::condensed`].
    scc_of: BTreeMap<u32, NodeIndex>,
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

/// This type is used to track the address of a resolved node. Because
/// Resolved nodes may be part of a cyclical group after petgraph has distilled
/// calls into "Strongly Connected Components", the node may actually be a
/// group of multiple functions at distinct addresses.
#[derive(Debug)]
pub enum NodeAddrKind {
    /// A single-function node without cycles
    Single(u32),
    /// A multi-function node that contains a cycle
    Cycle(Vec<u32>),
}

impl NodeAddrKind {
    fn fmt_addr(&self) -> String {
        match self {
            NodeAddrKind::Single(addr) => format!("0x{addr:08X}"),
            NodeAddrKind::Cycle(items) => {
                let items = items
                    .iter()
                    .map(|addr| format!("0x{addr:08X}"))
                    .collect::<Vec<_>>();
                format!("cycle[{}]", items.join(" <-> "))
            }
        }
    }
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
    let fns = extract_function_items(elf, verbose)?;
    let mut resolver = Resolver::new(fns.function_items, &config);
    let node = resolver.resolve_by_name("_start")?;
    let chain = node.worst_chain();
    let mut chain_compat = chain
        .into_iter()
        .map(|n| (n.local_size.unwrap_or(0), n.name.clone()))
        .collect::<Vec<_>>();

    let missing = resolver.find_missing_nodes(&["_start"])?;

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
            "- {} {} [+{frame_size:?} => {stack_depth}]",
            self.addr.fmt_addr(),
            self.name,
        );
        for child in self.children.iter() {
            child.debug_all_depth(
                depth + 1,
                current_stack + frame_size.unwrap_or(0),
            );
        }
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
            .find(|c| c.max_stack() == self.max_children)
        {
            child.worst_chain_inner(chain)
        }
    }
}

impl Resolver {
    /// Create a new resolver from the given map of [`FunctionData`] items
    pub fn new(fn_items: BTreeMap<u32, FunctionData>, config: &Config) -> Self {
        Self {
            all_resolved: BTreeMap::new(),
            fn_items,
            config: config.clone(),
            condensed: None,
            scc_of: BTreeMap::new(),
        }
    }

    /// Attempt to resolve a function by name into a [`ResolvedNode`].
    pub fn resolve_by_name(&mut self, entry: &str) -> Result<Rc<ResolvedNode>> {
        let addr = self.find_addr_by_name(entry)?;
        self.resolve_addr(addr)
    }

    /// Resolve a function by address into a [`ResolvedNode`].
    ///
    /// Note that if the function is a member of an allow-listed recursive
    /// cycle, the returned node represents the whole cycle, not just the
    /// requested function.
    pub fn resolve_addr(&mut self, addr: u32) -> Result<Rc<ResolvedNode>> {
        self.resolve_all()?;
        let Some(node) = self.all_resolved.get(&addr) else {
            bail!("no function data for {addr:08X}");
        };
        Ok(node.clone())
    }

    /// Resolve the entire call graph at once (idempotent).
    ///
    /// This works in three steps:
    ///
    /// 1. Build a directed graph where nodes are function base addresses and
    ///    an edge `A -> B` means "A contains a call to B".
    /// 2. Condense the graph: collapse every strongly connected component
    ///    (i.e. every recursive cycle) into a single node. Multi-function
    ///    nodes are only permitted if a member matches
    ///    [`Config::allowed_recurses`]; otherwise we refuse to continue. The
    ///    result is guaranteed to be acyclic.
    /// 3. Walk the condensed graph once in reverse topological order (deepest
    ///    callees first), so that every node's children are fully resolved
    ///    before the node itself, and record each node's max stack usage.
    ///
    /// Because recursion is handled per-cycle rather than per-call-site, the
    /// outcome does not depend on the order in which functions are visited,
    /// and a node memoized while breaking one cycle can never be observed
    /// with a truncated max stack by an unrelated caller.
    fn resolve_all(&mut self) -> Result<()> {
        if self.condensed.is_some() {
            return Ok(());
        }

        // Step 1: the function-level call graph.
        let mut fn_graph = DiGraphMap::<u32, ()>::new();
        for (addr, data) in &self.fn_items {
            fn_graph.add_node(*addr);
            for callee in &data.calls {
                if !self.fn_items.contains_key(callee) {
                    bail!(
                        "no function data for {callee:08X} \
                         (called from {})",
                        data.name
                    );
                }
                fn_graph.add_edge(*addr, *callee, ());
            }
        }

        // Step 2: condense cycles. Nodes of the condensed graph carry the
        // list of member function addresses; almost all of them will be
        // single-member. Passing `make_acyclic = true` removes the
        // internal edges of each cycle. We'll check self-recursion and intra-
        // condensed cycles manually below.
        //
        // "Condensed" nodes compresses any groups with cycles into a single
        // node entity, containing a vec of members.
        let condensed: Graph<Vec<u32>, ()> =
            condensation(fn_graph.into_graph::<u32>(), true);

        // We build a mapping between "function addresses" to "condensed node
        // indexes". Some functions may have the same "condensed node index",
        // as they have been grouped together by the `condensation` step above.
        let mut scc_of = BTreeMap::new();
        for idx in condensed.node_indices() {
            for addr in &condensed[idx] {
                scc_of.insert(*addr, idx);
            }
        }

        // Check each condensed node against the recursion policy
        for idx in condensed.node_indices() {
            let members = &condensed[idx];

            // Before we check if there are any recursion *cycles*, we need to
            // check if any functions are *self* recursive, e.g.:
            //
            // ```rust
            // fn a(x: bool) { if x { a(false) } }
            // ```
            //
            // Our previous call to `condensation` was called with
            // `make_acyclic`, meaning that self-recursive cycles will not
            // appear here. We will check for non-self-recursive cycles after
            // this check.
            for member in members.iter() {
                let this = &self.fn_items[member];
                if this.calls.contains(member) {
                    if let Some(am) =
                        self.config.match_allowed_recurses(&this.name)
                    {
                        // For now, pragmatically, we'll just ignore this
                        // recursive call site.
                        println!(
                            "WARN: Allowing {} to self-recurse, matching '{}'",
                            this.name, am
                        );
                    } else {
                        bail!(
                            "Refusing to handle self-recursion of {}",
                            this.name
                        );
                    }
                }
            }

            // If a function has a SINGLE member, then it does not have any
            // cycles, and we've already checked it is not self-recursive.
            if members.len() <= 1 {
                continue;
            }
            let names = members
                .iter()
                .map(|m| self.fn_items[m].name.as_str())
                .collect::<Vec<_>>();
            let allow_match = names
                .iter()
                .find_map(|n| self.config.match_allowed_recurses(n));
            if let Some(am) = allow_match {
                // We can't reason about how deep an allowed recursive cycle
                // actually goes, so we assume a single traversal: each
                // member's stack frame is counted exactly once (see step 3).
                println!(
                    "WARN: Allowing recursion between {{{}}}, matching '{}'",
                    names.join(", "),
                    am
                );
            } else {
                bail!(
                    "Refusing to handle recursion between: {}",
                    names.join(", ")
                );
            }
        }

        // Step 3.1: Sort the nodes topologically, meaning that we order
        // caller nodes before callee nodes (from "root" to "leaf" order)
        //
        // If we had `f(g(h()))`, we would visit f, g, h in that order.
        let order = toposort(&condensed, None).map_err(|_| {
            anyhow!("internal error: condensed call graph has a cycle")
        })?;

        // Step 3.2: We *reverse* the order, so we iterate from "leaf" to "root"
        // which means that we always visit a callee before we visit callers.
        //
        // If we had `f(g(h()))`, we would visit h, g, f in that order.
        //
        // This is useful, because we can now render the necessary stack usage
        // from the deepest part of the call stack first.
        let mut nodes = BTreeMap::<NodeIndex, Rc<ResolvedNode>>::new();
        for idx in order.iter().rev().copied() {
            let members = &condensed[idx];

            // The local stack usage of this node. For an allow-listed
            // recursive cycle, this is the sum of all members: any single
            // traversal of the cycle can visit each member at most once,
            // so the sum is a safe bound for the "recursion executes one
            // pass" assumption. We keep `None` (functions with no
            // `.stack_sizes` info) distinct from `Some(0)` for reporting.
            let mut local_size: Option<u64> = None;
            for m in members {
                if let Some(fs) = self.fn_items[m].frame_size {
                    *local_size.get_or_insert(0) += fs;
                }
            }

            // Get the name(s) and address(es) of the functions in this graph
            // node.
            let (name, addr) = if let [single] = members.as_slice() {
                let name = self.fn_items[single].name.clone();
                let addr = NodeAddrKind::Single(*single);
                (name, addr)
            } else {
                let mut names = members
                    .iter()
                    .map(|m| self.fn_items[m].name.as_str())
                    .collect::<Vec<_>>();
                names.sort_unstable();
                let name = format!("cycle[{}]", names.join(" <-> "));

                let mut addrs: Vec<_> = members.iter().copied().collect();
                addrs.sort_unstable();
                let addr = NodeAddrKind::Cycle(addrs);

                (name, addr)
            };

            // We now visit all "successors", which are nodes that are one
            // "outgoing" hop away from the current node idx. This is every
            // function that is *called by* the current function.
            let mut max_children = 0u64;
            let mut children = Vec::new();
            for succ in condensed.neighbors_directed(idx, Direction::Outgoing) {
                // Already resolved: successors precede us in reverse
                // topological order, so they've already been populated in
                // `nodes`.
                let child = &nodes[&succ];

                // Is this an ignored function? Note that for a cycle node,
                // the composite name contains every member's name, so
                // ignoring any member ignores the whole cycle.
                let ignore_match =
                    self.config.match_ignored_functions(&child.name);
                if let Some(ignore) = ignore_match {
                    println!(
                        "WARN: Ignoring {}, matching '{}'",
                        child.name, ignore
                    );
                    continue;
                }

                // Not ignored: consider this child node as part of the max
                // stack analysis
                max_children = max_children.max(child.max_stack());
                children.push(child.clone());
            }

            let node = Rc::new(ResolvedNode {
                addr,
                name,
                local_size,
                max_children,
                children,
            });
            nodes.insert(idx, node);
        }

        for (addr, idx) in &scc_of {
            self.all_resolved.insert(*addr, nodes[idx].clone());
        }
        self.condensed = Some(condensed);
        self.scc_of = scc_of;
        Ok(())
    }

    /// Finds the address of a function by exact function name
    fn find_addr_by_name(&self, entry: &str) -> Result<u32> {
        self.fn_items
            .iter()
            .find_map(
                |(addr, v)| {
                    if v.name == *entry { Some(*addr) } else { None }
                },
            )
            .ok_or_else(|| anyhow!("function '{entry}' not found"))
    }

    /// Finds all functions *not* reachable from the given list of entry points
    ///
    /// This finds functions that exist in the elf file, but were not visited
    /// while resolving the call graph, likely due to being called indirectly or
    /// through vtable methods.
    fn find_missing_nodes(
        &mut self,
        entrypoints: &[&str],
    ) -> Result<Vec<Rc<ResolvedNode>>> {
        self.resolve_all()?;
        let condensed = self.condensed.as_ref().unwrap();

        // Find every node reachable from the previously-requested entry;
        // those are already accounted for in the entry's max stacks.
        let mut reached = BTreeSet::new();
        for entry in entrypoints {
            let addr = self.find_addr_by_name(entry)?;
            let mut dfs = Dfs::new(condensed, self.scc_of[&addr]);
            while let Some(idx) = dfs.next(condensed) {
                reached.insert(idx);
            }
        }

        // Everything else is "missing". Deduplicate cycles (whose members
        // all share one node) by keying on the node's address, and filter
        // out any ignored functions.
        let mut found = Vec::new();
        for (addr, idx) in &self.scc_of {
            if reached.contains(idx) {
                continue;
            }
            let node = &self.all_resolved[addr];
            if let Some(ignore) =
                self.config.match_ignored_functions(&node.name)
            {
                println!("WARN: Ignoring {}, matching '{}'", node.name, ignore);
                continue;
            }
            found.push(node.clone());
        }

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
                self.calls.insert(target);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(name: &str, frame: u64, calls: &[u32]) -> FunctionData {
        FunctionData {
            name: name.to_string(),
            frame_size: Some(frame),
            calls: calls.iter().copied().collect(),
            missing_calls: 0,
            recursive_calls: 0,
        }
    }

    fn config(allow: &[&str], ignore: &[&str]) -> Config {
        Config {
            allowed_recurses: allow.iter().map(|s| s.to_string()).collect(),
            ignored_functions: ignore.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn resolver(fns: &[(u32, FunctionData)], config: &Config) -> Resolver {
        Resolver::new(fns.iter().cloned().collect(), config)
    }

    /// A shared child in a diamond must be counted along both paths, and the
    /// worst chain must pick the deeper one.
    #[test]
    fn diamond() {
        let mut r = resolver(
            &[
                (0x1000, fd("start", 0, &[0x2000, 0x3000])),
                (0x2000, fd("deep", 100, &[0x4000])),
                (0x3000, fd("shallow", 20, &[0x4000])),
                (0x4000, fd("shared", 10, &[])),
            ],
            &config(&[], &[]),
        );
        let root = r.resolve_by_name("start").unwrap();
        assert_eq!(root.max_stack(), 110);
        let names = root
            .worst_chain()
            .iter()
            .map(|n| n.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, ["start", "deep", "shared"]);
        // The chain frames must sum to the root's max stack.
        let sum: u64 = root
            .worst_chain()
            .iter()
            .map(|n| n.local_size.unwrap_or(0))
            .sum();
        assert_eq!(sum, root.max_stack());
    }

    /// A call to an address with no function data must be an error naming
    /// the caller.
    #[test]
    fn unknown_callee() {
        let mut r =
            resolver(&[(0x1000, fd("start", 0, &[0x9999]))], &config(&[], &[]));
        let err = r.resolve_by_name("start").unwrap_err().to_string();
        assert!(err.contains("00009999"), "{err}");
        assert!(err.contains("start"), "{err}");
    }

    /// An allow-listed cycle `alpha <-> beta` with a second, non-recursive
    /// caller of `beta` must produce the same (safe) answer regardless of
    /// address order. This is a regression test: a resolver that memoizes
    /// per-path cycle-breaking underestimates the `start -> other -> beta ->
    /// alpha` chain, or fails outright, depending on which caller resolves
    /// first.
    #[test]
    fn allowed_cycle_is_order_independent() {
        // start(0) -> { alpha(100), other(20) }
        // alpha(100) <-> beta(10), allow-listed
        // other(20) -> beta(10)
        //
        // Worst case with "one traversal of the cycle" semantics:
        // start -> other -> (alpha <-> beta) = 0 + 20 + 110 = 130.
        for (alpha, beta, other) in
            [(0x2000, 0x3000, 0x4000), (0x4000, 0x3000, 0x2000)]
        {
            let mut r = resolver(
                &[
                    (0x1000, fd("start", 0, &[alpha, other])),
                    (alpha, fd("alpha", 100, &[beta])),
                    (beta, fd("beta", 10, &[alpha])),
                    (other, fd("other", 20, &[beta])),
                ],
                &config(&["alpha"], &[]),
            );
            let root = r.resolve_by_name("start").unwrap();
            assert_eq!(root.max_stack(), 130);
        }
    }

    /// A cycle with no allow-list match must fail, and the error must name
    /// every member of the cycle.
    #[test]
    fn disallowed_cycle_names_all_members() {
        let mut r = resolver(
            &[
                (0x1000, fd("start", 0, &[0x2000])),
                (0x2000, fd("alpha", 100, &[0x3000])),
                (0x3000, fd("beta", 10, &[0x2000])),
            ],
            &config(&[], &[]),
        );
        let err = r.resolve_by_name("start").unwrap_err().to_string();
        assert!(err.contains("alpha"), "{err}");
        assert!(err.contains("beta"), "{err}");
    }

    /// Ignored functions contribute nothing to their callers.
    #[test]
    fn ignored_functions_do_not_contribute() {
        let mut r = resolver(
            &[
                (0x1000, fd("start", 0, &[0x2000, 0x3000])),
                (0x2000, fd("stackblow", 8192, &[])),
                (0x3000, fd("normal", 32, &[])),
            ],
            &config(&[], &["stackblow"]),
        );
        let root = r.resolve_by_name("start").unwrap();
        assert_eq!(root.max_stack(), 32);
    }

    /// Functions unreachable from the resolved roots are reported as
    /// missing (each with its own subtree max), and ignored functions are
    /// excluded from that report.
    #[test]
    fn missing_nodes() {
        let mut r = resolver(
            &[
                (0x1000, fd("start", 0, &[])),
                (0x2000, fd("orphan", 500, &[0x3000])),
                (0x3000, fd("leaf", 20, &[])),
            ],
            &config(&[], &[]),
        );
        r.resolve_by_name("start").unwrap();
        let missing = r.find_missing_nodes(&["start"]).unwrap();
        let max = missing.iter().map(|n| n.max_stack()).max().unwrap();
        assert_eq!(max, 520);

        // Same graph, but with the orphan ignored: only the leaf remains.
        let mut r = resolver(
            &[
                (0x1000, fd("start", 0, &[])),
                (0x2000, fd("orphan", 500, &[0x3000])),
                (0x3000, fd("leaf", 20, &[])),
            ],
            &config(&[], &["orphan"]),
        );
        r.resolve_by_name("start").unwrap();
        let missing = r.find_missing_nodes(&["start"]).unwrap();
        let max = missing.iter().map(|n| n.max_stack()).max().unwrap();
        assert_eq!(max, 20);
    }
}
