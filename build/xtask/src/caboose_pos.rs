// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::elf;
use anyhow::{bail, Context, Result};
use scroll::Pread;

pub const CABOOSE_POS_TABLE_SECTION: &str = ".caboose_pos_table";

#[derive(Debug)]
pub struct CaboosePosTableEntry {
    pub caboose_pos_address: u64,
    pub caboose_pos_file_offset: u64,
}

impl scroll::ctx::TryFromCtx<'_, &goblin::elf::Elf<'_>>
    for CaboosePosTableEntry
{
    type Error = anyhow::Error;

    fn try_from_ctx(
        src: &[u8],
        elf: &goblin::elf::Elf,
    ) -> Result<(Self, usize), Self::Error> {
        let endianness = elf::get_endianness(elf);
        let src_offset = &mut 0;

        let caboose_pos_address = if elf.is_64 {
            src.gread_with::<u64>(src_offset, endianness)?
        } else {
            src.gread_with::<u32>(src_offset, endianness)? as u64
        };

        let caboose_pos_file_offset =
            crate::elf::get_file_offset_by_vma(elf, caboose_pos_address)
                .context("could not get caboose pos file offset")?;

        Ok((
            Self {
                caboose_pos_address,
                caboose_pos_file_offset,
            },
            *src_offset,
        ))
    }
}

pub fn get_caboose_pos_table_entry(
    src: &[u8],
    elf: &goblin::elf::Elf,
) -> Result<Option<CaboosePosTableEntry>> {
    // If the section isn't present, then we're not reading the caboose position
    // from this task.
    let Some(caboose_pos_table_section) = elf::get_section_by_name(
        elf, CABOOSE_POS_TABLE_SECTION
    ) else {
        return Ok(None);
    };

    let caboose_pos_table = &src[caboose_pos_table_section.sh_offset as usize
        ..(caboose_pos_table_section.sh_offset
            + caboose_pos_table_section.sh_size) as usize];

    let mut entries = Vec::<CaboosePosTableEntry>::new();
    let cur_offset = &mut 0;

    while *cur_offset < caboose_pos_table.len() {
        let x = caboose_pos_table
            .gread_with::<CaboosePosTableEntry>(cur_offset, elf)?;
        entries.push(x);
    }

    match entries.len() {
        0 => Ok(None),
        1 => Ok(entries.pop()),
        i => bail!(
            "expected one entry in {CABOOSE_POS_TABLE_SECTION}, found {i}"
        ),
    }
}
