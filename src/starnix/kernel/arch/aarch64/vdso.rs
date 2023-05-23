// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use process_builder::elf_parse;
use zerocopy::AsBytes;

use crate::types::{errno, from_status_like_fdio, Errno};

pub const HAS_VDSO: bool = true;

pub fn set_vdso_constants(_vdso_vmo: &zx::Vmo) -> Result<(), Errno> {
    Ok(())
}

pub fn get_sigreturn_offset(vdso_vmo: &zx::Vmo) -> Result<Option<u64>, Errno> {
    let dyn_section = elf_parse::Elf64DynSection::from_vmo(vdso_vmo).map_err(|_| errno!(EINVAL))?;
    let symtab =
        dyn_section.dynamic_entry_with_tag(elf_parse::Elf64DynTag::Symtab).ok_or(errno!(EINVAL))?;
    let strtab =
        dyn_section.dynamic_entry_with_tag(elf_parse::Elf64DynTag::Strtab).ok_or(errno!(EINVAL))?;
    let strsz =
        dyn_section.dynamic_entry_with_tag(elf_parse::Elf64DynTag::Strsz).ok_or(errno!(EINVAL))?;

    const SIGRETURN_NAME: &str = "__kernel_rt_sigreturn";

    // Find the name of the signal trampoline in the string table and store the index.
    let mut strtab_bytes = vec![0u8; strsz.value as usize];
    vdso_vmo
        .read(&mut strtab_bytes, strtab.value)
        .map_err(|status| from_status_like_fdio!(status))?;
    let mut strtab_items = strtab_bytes.split(|c: &u8| *c == 0u8);
    let strtab_idx = strtab_items
        .position(|entry: &[u8]| std::str::from_utf8(entry) == Ok(SIGRETURN_NAME))
        .ok_or(errno!(ENOENT))?;

    const SYM_ENTRY_SIZE: usize = std::mem::size_of::<elf_parse::Elf64Sym>();

    // In the symbolic table, find a symbol with a name index pointing to the name we're looking for.
    let mut symtab_offset = symtab.value;
    loop {
        let mut sym_entry = elf_parse::Elf64Sym::default();
        vdso_vmo
            .read(sym_entry.as_bytes_mut(), symtab_offset)
            .map_err(|status| from_status_like_fdio!(status))?;
        if sym_entry.st_name as usize == strtab_idx {
            return Ok(Some(sym_entry.st_value));
        }
        symtab_offset += SYM_ENTRY_SIZE as u64;
    }
}
