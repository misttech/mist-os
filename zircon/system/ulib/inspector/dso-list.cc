// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <elf-search.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <link.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include "dso-list-impl.h"
#include "inspector/inspector.h"
#include "lib/stdcompat/functional.h"
#include "utils-impl.h"

namespace {

bool ModuleContainsFrameAddress(const elf_search::ModuleInfo& info, const uint64_t pc) {
  auto contains_frame_address = [pc, &info](const Elf64_Phdr& phdr) {
    uintptr_t start = phdr.p_vaddr;
    uintptr_t end = phdr.p_vaddr + phdr.p_memsz;

    return pc >= info.vaddr + start && pc < info.vaddr + end;
  };

  return std::any_of(info.phdrs.begin(), info.phdrs.end(), contains_frame_address);
}

}  // namespace

namespace inspector {

void print_markup_context(FILE* f, zx_handle_t process, cpp20::span<uint64_t> pcs) {
  fprintf(f, "{{{reset}}}\n");
  elf_search::ForEachModule(
      *zx::unowned_process{process},
      [f, &pcs, count = 0u](const elf_search::ModuleInfo& info) mutable {
        auto is_frame_from_module = [&info](const uint64_t pc) {
          return ModuleContainsFrameAddress(info, pc);
        };
        if (!pcs.empty() && std::none_of(pcs.begin(), pcs.end(), is_frame_from_module)) {
          return;
        }

        const size_t kPageSize = zx_system_get_page_size();
        unsigned int module_id = count++;
        // Print out the module first.
        fprintf(f, "{{{module:%#x:%s:elf:", module_id, info.name.begin());
        for (uint8_t byte : info.build_id) {
          fprintf(f, "%02x", byte);
        }
        fprintf(f, "}}}\n");
        // Now print out the various segments.
        for (const auto& phdr : info.phdrs) {
          if (phdr.p_type != PT_LOAD) {
            continue;
          }
          uintptr_t start = phdr.p_vaddr & -kPageSize;
          uintptr_t end = (phdr.p_vaddr + phdr.p_memsz + kPageSize - 1) & -kPageSize;
          fprintf(f, "{{{mmap:%#" PRIxPTR ":%#" PRIxPTR ":load:%#x:", info.vaddr + start,
                  end - start, module_id);
          if (phdr.p_flags & PF_R) {
            fprintf(f, "%c", 'r');
          }
          if (phdr.p_flags & PF_W) {
            fprintf(f, "%c", 'w');
          }
          if (phdr.p_flags & PF_X) {
            fprintf(f, "%c", 'x');
          }
          fprintf(f, ":%#" PRIxPTR "}}}\n", start);
        }
      });
}

}  // namespace inspector

__EXPORT void inspector_print_markup_context(FILE* f, zx_handle_t process) {
  inspector::print_markup_context(f, process, {});
}
