// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_ELF_H_
#define ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_ELF_H_

#include <lib/elfldltl/layout.h>
#include <lib/mistos/zbi_parser/bootfs.h>
#include <lib/mistos/zx/debuglog.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <zircon/types.h>

#include <fbl/string.h>
#include <fbl/vector.h>

struct LoadedElf {
  elfldltl::Elf<>::Ehdr header;
  zx::vmo vmo;
  zx_vaddr_t base = 0;
  zx_vaddr_t load_bias = 0;
};

struct ElfInfo {
  LoadedElf main_elf;

  // TODO use klt::optional
  LoadedElf interp_elf;
  bool has_interp = false;

  fbl::Vector<fbl::String> argv;
  fbl::Vector<fbl::String> envp;
};

struct StackResult {
  zx_vaddr_t stack_pointer;
  zx_vaddr_t auxv_start;
  zx_vaddr_t auxv_end;
  zx_vaddr_t argv_start;
  zx_vaddr_t argv_end;
  zx_vaddr_t environ_start;
  zx_vaddr_t environ_end;
};

const size_t kRandomSeedBytes = 16;

class Bootfs;

zx_vaddr_t elf_load_bootfs(const zx::debuglog& log, zbi_parser::Bootfs& fs, ktl::string_view root,
                           const zx::vmar& vmar, ktl::string_view filename, size_t* stack_size,
                           ElfInfo* info);

size_t get_initial_stack_size(const fbl::String& path, const fbl::Vector<fbl::String>& argv,
                              const fbl::Vector<fbl::String>& environ,
                              const fbl::Vector<ktl::pair<uint32_t, uint64_t>>& auxv);

fit::result<zx_status_t, StackResult> populate_initial_stack(
    const zx::debuglog& log, zx::vmo& stack_vmo, const fbl::String& path,
    const fbl::Vector<fbl::String>& argv, const fbl::Vector<fbl::String>& envp,
    fbl::Vector<std::pair<uint32_t, uint64_t>>& auxv, zx_vaddr_t mapping_base,
    zx_vaddr_t original_stack_start_addr);

#endif  // ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_INCLUDE_LIB_MISTOS_USERLOADER_ELF_H_
