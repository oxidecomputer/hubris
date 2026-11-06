// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! ## Static Memory Access Analysis
//!
//! Tasks are only allowed to touch a limited set of memory regions (their own
//! flash and RAM, plus any peripherals / extern regions listed in the
//! manifest).  If a task touches memory outside of those regions, it will
//! fault at runtime; this usually indicates a missing `uses = [...]` entry in
//! the manifest.
//!
//! This crate catches a subset of those failures at build time, by
//! disassembling the task's `.text` section and looking for a very specific
//! pattern:
//!
//! 1. A PC-relative `ldr` which loads a constant (typically an address) from a
//!    literal pool into a register
//! 2. A later memory access (`ldr`, `str`, and friends) which uses that
//!    register as its base address, with an immediate offset
//!
//! This is a deliberately simple analysis, which tracks known register values
//! within straight-line code.  It is designed to have false negatives rather
//! than false positives:
//!
//! - Known values are discarded at every branch target and after every
//!   unconditional branch (since we don't know the state of registers coming
//!   from elsewhere)
//! - Function calls discard values in caller-saved registers (`r0-r3`, `r12`)
//! - Any instruction which mentions a register as an operand is assumed to
//!   clobber it (capstone does not tell us about per-operand access for ARM)
//! - Conditional writes (e.g. in an `IT` block) discard the known value
//!
//! Notably, addresses built with `movw` / `movt` pairs or arithmetic are not
//! detected.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    path::Path,
};

use anyhow::{Context, Result, anyhow};
use capstone::{
    Capstone, Insn, InsnDetail, InsnGroupId, InsnGroupType, RegId,
    arch::{
        ArchDetail, BuildsCapstone, BuildsCapstoneExtraMode, DetailsArchInsn,
        arm::{self, ArmCC, ArmInsn, ArmOperandType, ArmReg},
    },
};
use goblin::elf::{Elf, section_header::SHT_NOBITS, sym::STT_FUNC};

/// A named range of memory which a task is allowed to access
#[derive(Debug, Clone)]
pub struct Region {
    pub name: String,
    /// Address range; this is a `u64` so that regions which end at the top of
    /// the 32-bit address space are representable.
    pub range: Range<u64>,
}

/// A memory access with a statically-known address
#[derive(Debug, Clone)]
pub struct MemoryAccess {
    /// Name of the function containing the access (demangled)
    pub function: String,
    /// Address of the instruction which performs the access
    pub pc: u32,
    /// Address being accessed
    pub addr: u32,
    /// Disassembled instruction (for human consumption)
    pub instr: String,
}

/// Finds memory accesses in the given ELF file which hit an address outside of
/// `allowed`.
pub fn find_bad_accesses(
    elf: &Path,
    allowed: &[Region],
) -> Result<Vec<MemoryAccess>> {
    let mut out = find_static_accesses(elf)?;
    out.retain(|a| {
        !allowed.iter().any(|r| r.range.contains(&u64::from(a.addr)))
    });
    Ok(out)
}

/// Finds all memory accesses in the given ELF file whose address can be
/// statically determined, using the pattern described in the crate docs.
pub fn find_static_accesses(elf: &Path) -> Result<Vec<MemoryAccess>> {
    let data = std::fs::read(elf).with_context(|| {
        format!("could not open ELF file: {}", elf.display())
    })?;
    let parsed = Elf::parse(&data)
        .with_context(|| format!("could not parse {}", elf.display()))?;

    let text = build_elf::get_section_by_name(&parsed, ".text")
        .context("could not get .text")?;
    let text_range = text.sh_addr..text.sh_addr + text.sh_size;

    let cs = Capstone::new()
        .arm()
        .mode(arm::ArchMode::Thumb)
        .extra_mode(std::iter::once(arm::ArchExtraMode::MClass))
        .detail(true)
        .build()
        .map_err(|e| anyhow!("failed to initialize disassembler: {e:?}"))?;

    let functions = function_names(&parsed);

    // Disassemble every chunk of code in the text section
    let mut chunks = vec![];
    for r in code_ranges(&parsed, text_range.clone()) {
        let offset = (r.start - text.sh_addr + text.sh_offset) as usize;
        let size = (r.end - r.start) as usize;
        let instrs = cs
            .disasm_all(&data[offset..][..size], r.start)
            .map_err(|e| anyhow!("disassembly failed: {e:?}"))?;
        chunks.push(instrs);
    }

    // First pass: find every direct branch target, because register state is
    // unknown when arriving at those instructions.
    let mut branch_targets = BTreeSet::new();
    for instr in chunks.iter().flat_map(|c| c.iter()) {
        let detail = cs
            .insn_detail(instr)
            .map_err(|e| anyhow!("could not get instruction details: {e}"))?;
        if !is_branch(&detail) {
            continue;
        }
        for op in arm_detail(&detail).operands() {
            if let ArmOperandType::Imm(t) = op.op_type {
                branch_targets.insert(t as u32 as u64);
            }
        }
    }

    // Second pass: track constant values loaded into registers and look for
    // memory accesses which use them.
    let mut out = vec![];
    for chunk in &chunks {
        let mut state = State::default();
        for instr in chunk.iter() {
            if branch_targets.contains(&instr.address()) {
                state.clear();
            }
            let detail = cs.insn_detail(instr).map_err(|e| {
                anyhow!("could not get instruction details: {e}")
            })?;
            if let Some(addr) = state.process(instr, &detail, &parsed, &data) {
                let pc = instr.address() as u32;
                let function = functions
                    .range(..=pc)
                    .next_back()
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| format!("<unknown@{pc:#010x}>"));
                out.push(MemoryAccess {
                    function,
                    pc,
                    addr,
                    instr: format!(
                        "{} {}",
                        instr.mnemonic().unwrap_or("???"),
                        instr.op_str().unwrap_or("")
                    ),
                });
            }
        }
    }

    Ok(out)
}

/// Register state within straight-line code
#[derive(Default)]
struct State {
    /// Map from register to known value
    known: BTreeMap<RegId, u32>,
}

impl State {
    fn clear(&mut self) {
        self.known.clear();
    }

    /// Processes a single instruction, updating our register state.
    ///
    /// Returns the address if this instruction accesses memory at a
    /// statically-known address.
    fn process(
        &mut self,
        instr: &Insn<'_>,
        detail: &InsnDetail<'_>,
        elf: &Elf<'_>,
        data: &[u8],
    ) -> Option<u32> {
        let arm = arm_detail(detail);
        let ops = arm.operands().collect::<Vec<_>>();
        let id = instr.id().0;
        let conditional = !matches!(arm.cc(), ArmCC::ARM_CC_AL)
            && !matches!(arm.cc(), ArmCC::ARM_CC_INVALID);

        // Check for memory accesses through a register with a known value.
        // Preload instructions are hints and don't fault, so we skip them.
        let mut access = None;
        let is_preload = id == ArmInsn::ARM_INS_PLD as u32
            || id == ArmInsn::ARM_INS_PLDW as u32
            || id == ArmInsn::ARM_INS_PLI as u32;
        if !is_preload {
            for op in &ops {
                let ArmOperandType::Mem(mem) = op.op_type else {
                    continue;
                };
                if mem.index() != reg(ArmReg::ARM_REG_INVALID) {
                    continue;
                }
                if let Some(base) = self.known.get(&mem.base()) {
                    let addr = base.wrapping_add_signed(disp(op, mem.disp()));
                    access = Some(addr);
                }
            }
        }

        // Check for a literal pool load, i.e. `ldr rX, [pc, #imm]`
        let mut literal = None;
        if id == ArmInsn::ARM_INS_LDR as u32
            && let [dst, src] = ops.as_slice()
            && let ArmOperandType::Reg(dst) = dst.op_type
            && let ArmOperandType::Mem(mem) = src.op_type
            && mem.base() == reg(ArmReg::ARM_REG_PC)
            && mem.index() == reg(ArmReg::ARM_REG_INVALID)
        {
            // In Thumb mode, PC reads as the address of the current
            // instruction plus 4, aligned down to a word boundary.
            let pc = (instr.address() as u32).wrapping_add(4) & !3;
            let addr = pc.wrapping_add_signed(disp(src, mem.disp()));
            if let Some(value) = read_u32(elf, data, addr) {
                literal = Some((dst, value));
            }
        }

        // Discard every register that this instruction could have written.
        // We don't know which register operands are written, so we
        // conservatively assume that they all are.
        for r in detail.regs_write() {
            self.known.remove(r);
        }
        for op in &ops {
            match op.op_type {
                ArmOperandType::Reg(r) => {
                    self.known.remove(&r);
                }
                ArmOperandType::Mem(mem) if arm.writeback() => {
                    self.known.remove(&mem.base());
                }
                _ => (),
            }
        }

        if is_branch(detail) {
            if is_call(detail) {
                // Calls clobber caller-saved registers, but should preserve
                // r4-r11 according to the AAPCS.
                for r in [
                    ArmReg::ARM_REG_R0,
                    ArmReg::ARM_REG_R1,
                    ArmReg::ARM_REG_R2,
                    ArmReg::ARM_REG_R3,
                    ArmReg::ARM_REG_R12,
                    ArmReg::ARM_REG_LR,
                ] {
                    self.known.remove(&reg(r));
                }
            } else if !conditional
                && id != ArmInsn::ARM_INS_CBZ as u32
                && id != ArmInsn::ARM_INS_CBNZ as u32
            {
                // After an unconditional branch, the next instruction is only
                // reachable from elsewhere.
                self.clear();
            }
        }

        // Record the literal load, unless it was conditional (in which case
        // the register value is unknown)
        if let Some((dst, v)) = literal
            && !conditional
        {
            self.known.insert(dst, v);
        }

        access
    }
}

fn reg(r: arm::ArmReg::Type) -> RegId {
    RegId(r as u16)
}

fn arm_detail<'a>(detail: &'a InsnDetail<'a>) -> arm::ArmInsnDetail<'a> {
    let ArchDetail::ArmDetail(arm) = detail.arch_detail() else {
        panic!("unsupported arch");
    };
    arm
}

/// Returns the signed displacement for a memory operand
fn disp(op: &arm::ArmOperand, disp: i32) -> i32 {
    // Capstone may represent negative offsets as a positive displacement with
    // the `subtracted` flag set.
    if op.subtracted && disp > 0 {
        -disp
    } else {
        disp
    }
}

fn has_group(detail: &InsnDetail<'_>, g: InsnGroupType::Type) -> bool {
    detail.groups().contains(&InsnGroupId(g as u8))
}

fn is_call(detail: &InsnDetail<'_>) -> bool {
    has_group(detail, InsnGroupType::CS_GRP_CALL)
}

/// Checks whether the instruction can change control flow
fn is_branch(detail: &InsnDetail<'_>) -> bool {
    has_group(detail, InsnGroupType::CS_GRP_JUMP)
        || has_group(detail, InsnGroupType::CS_GRP_CALL)
        || has_group(detail, InsnGroupType::CS_GRP_RET)
        || has_group(detail, InsnGroupType::CS_GRP_IRET)
        || has_group(detail, InsnGroupType::CS_GRP_INT)
        || has_group(detail, InsnGroupType::CS_GRP_BRANCH_RELATIVE)
        || detail.regs_write().contains(&reg(ArmReg::ARM_REG_PC))
        || arm_detail(detail).operands().any(|op| {
            op.op_type == ArmOperandType::Reg(reg(ArmReg::ARM_REG_PC))
        })
}

/// Reads a little-endian `u32` from initialized data in the ELF file
fn read_u32(elf: &Elf<'_>, data: &[u8], addr: u32) -> Option<u32> {
    let addr = u64::from(addr);
    let s = elf.section_headers.iter().find(|s| {
        s.sh_type != SHT_NOBITS
            && s.sh_addr != 0
            && (s.sh_addr..s.sh_addr + s.sh_size).contains(&addr)
            && (s.sh_addr..s.sh_addr + s.sh_size).contains(&(addr + 3))
    })?;
    let offset = (addr - s.sh_addr + s.sh_offset) as usize;
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().unwrap()))
}

/// Builds a map from function address to demangled name
fn function_names(elf: &Elf<'_>) -> BTreeMap<u32, String> {
    elf.syms
        .iter()
        .filter(|s| s.st_type() == STT_FUNC && s.st_name != 0)
        .filter_map(|s| {
            let name = elf.strtab.get_at(s.st_name)?;
            Some((
                (s.st_value & !1) as u32,
                rustc_demangle::demangle(name).to_string(),
            ))
        })
        .collect()
}

/// Returns ranges within the text section which contain code (rather than
/// inline data), based on ARM mapping symbols (`$t` / `$d` / `$a`).
fn code_ranges(elf: &Elf<'_>, text: Range<u64>) -> Vec<Range<u64>> {
    let mut mapping = BTreeMap::new();
    for s in elf.syms.iter() {
        if !text.contains(&s.st_value) {
            continue;
        }
        let Some(name) = elf.strtab.get_at(s.st_name) else {
            continue;
        };
        let is_code = match name.split('.').next() {
            Some("$t") => true,
            Some("$d" | "$a") => false,
            _ => continue,
        };
        // If a code and data marker are at the same address, data wins
        mapping
            .entry(s.st_value)
            .and_modify(|c| *c &= is_code)
            .or_insert(is_code);
    }

    let mut out = vec![];
    let mut start = None;
    for (addr, is_code) in mapping.into_iter().chain([(text.end, false)]) {
        match (start, is_code) {
            (None, true) => start = Some(addr),
            (Some(s), false) => {
                out.push(s..addr);
                start = None;
            }
            _ => (),
        }
    }
    out
}
