// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <elf-search.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

using callback = void (*)(uint64_t address, uint64_t size, uint64_t file_offset,
                          const uint8_t* build_id, size_t build_id_len, uint64_t vaddr,
                          const char* name, size_t name_len, void* callback_arg);

// Calls the given `callback` for each ELF executable segment in the given process.
extern "C" __EXPORT zx_status_t ElfSearchExecutableRegions(zx_handle_t process_handle,
                                                           callback callback, void* callback_arg) {
  zx::unowned_process process(process_handle);

  return elf_search::ForEachModule(*process, [&](const elf_search::ModuleInfo& info) {
    for (const Elf64_Phdr& phdr : info.phdrs) {
      if (phdr.p_type != PT_LOAD || (phdr.p_flags & PF_X) == 0)
        continue;
      callback(info.vaddr + phdr.p_vaddr, phdr.p_memsz, phdr.p_offset, info.build_id.data(),
               info.build_id.size(), phdr.p_vaddr, info.name.data(), info.name.length(),
               callback_arg);
    }
  });
}
