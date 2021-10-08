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
    for section in &elf.section_headers {
        if addr >= section.sh_addr && addr < (section.sh_addr + section.sh_size)
        {
            return Some(section);
        }
    }
    None
}
