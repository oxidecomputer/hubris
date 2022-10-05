// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub fn get_endianness(elf: &goblin::elf::Elf) -> scroll::Endian {
    if elf.little_endian {
        scroll::Endian::Little
    } else {
        scroll::Endian::Big
    }
}

pub fn get_section_by_name<'a>(
    elf: &'a goblin::elf::Elf,
    name: &str,
) -> Option<&'a goblin::elf::SectionHeader> {
    for section in &elf.section_headers {
        if let Some(section_name) = elf.shdr_strtab.get_at(section.sh_name) {
            if section_name == name {
                return Some(section);
            }
        }
    }
    None
}

pub fn get_section_by_vma<'a>(
    elf: &'a goblin::elf::Elf,
    addr: u64,
) -> Option<&'a goblin::elf::SectionHeader> {
    elf.section_headers.iter().find(|&section| {
        addr >= section.sh_addr && addr < (section.sh_addr + section.sh_size)
    })
}
