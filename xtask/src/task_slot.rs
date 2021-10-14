use crate::elf;
use anyhow::{bail, Result};
use scroll::Pread;
use std::path::PathBuf;

pub const TASK_SLOT_TABLE_SECTION: &'static str = ".task_slot_table";

#[derive(Debug)]
pub struct TaskSlotTableEntry<'a> {
    pub taskidx_address: u64,
    pub taskidx_file_offset: u64,
    pub slot_name: &'a str,
}

impl<'a> scroll::ctx::TryFromCtx<'a, &goblin::elf::Elf<'a>>
    for TaskSlotTableEntry<'a>
{
    type Error = anyhow::Error;

    fn try_from_ctx(
        src: &'a [u8],
        elf: &goblin::elf::Elf<'a>,
    ) -> Result<(Self, usize), Self::Error> {
        let endianness = elf::get_endianness(&elf);
        let src_offset = &mut 0;

        let taskidx_address = if elf.is_64 {
            src.gread_with::<u64>(src_offset, endianness)?
        } else {
            src.gread_with::<u32>(src_offset, endianness)? as u64
        };

        let slot_name_len = if elf.is_64 {
            src.gread_with::<u64>(src_offset, endianness)? as usize
        } else {
            src.gread_with::<u32>(src_offset, endianness)? as usize
        };

        let slot_name: &str = src.gread_with(
            src_offset,
            scroll::ctx::StrCtx::Length(slot_name_len),
        )?;

        let entry_section =
            match crate::elf::get_section_by_vma(&elf, taskidx_address) {
                Some(x) => x,
                _ => bail!(
                    "slot '{}' points to a non-existant address 0x{:x}",
                    slot_name,
                    taskidx_address
                ),
            };

        let taskidx_file_offset =
            taskidx_address - entry_section.sh_addr + entry_section.sh_offset;

        Ok((
            Self {
                taskidx_address: taskidx_address,
                taskidx_file_offset: taskidx_file_offset,
                slot_name: slot_name,
            },
            *src_offset,
        ))
    }
}

pub fn get_task_slot_table_entries<'a>(
    src: &'a [u8],
    elf: &goblin::elf::Elf<'a>,
) -> Result<Vec<TaskSlotTableEntry<'a>>> {
    let task_slot_table_section =
        match elf::get_section_by_name(&elf, TASK_SLOT_TABLE_SECTION) {
            Some(task_slot_table_section) => task_slot_table_section,
            _ => bail!("No {} section", TASK_SLOT_TABLE_SECTION),
        };

    let task_slot_table = &src[task_slot_table_section.sh_offset as usize
        ..(task_slot_table_section.sh_offset + task_slot_table_section.sh_size)
            as usize];

    let mut entries = Vec::<TaskSlotTableEntry>::new();
    let cur_offset = &mut 0;

    while *cur_offset < task_slot_table.len() {
        match task_slot_table.gread_with::<TaskSlotTableEntry>(cur_offset, elf)
        {
            Ok(x) => entries.push(x),
            Err(x) => return Err(x.into()),
        }
    }

    Ok(entries)
}

pub fn dump_task_slot_table(task_path: &PathBuf) -> Result<()> {
    let task_bin = std::fs::read(task_path)?;
    let elf = goblin::elf::Elf::parse(&task_bin)?;

    println!("Task Slot          Address      File Offset   Task Index");
    println!("------------------------------------------------------------");

    for entry in get_task_slot_table_entries(&task_bin, &elf)? {
        let task_idx = task_bin.pread_with::<u16>(
            entry.taskidx_file_offset as usize,
            elf::get_endianness(&elf),
        )?;

        println!(
            "{:16}   0x{:08x}   0x{:08x}    0x{:04x}",
            entry.slot_name,
            entry.taskidx_address,
            entry.taskidx_file_offset as usize,
            task_idx
        );
    }

    Ok(())
}
