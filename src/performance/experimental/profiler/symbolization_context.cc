// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "symbolization_context.h"

#include <elf-search.h>

#include <algorithm>
#include <iterator>

zx::result<std::vector<profiler::Module>> profiler::GetProcessModules(
    const zx::unowned_process& process) {
  std::vector<profiler::Module> modules;
  zx_status_t search_result = elf_search::ForEachModule(
      *process, [&modules, count = 0u](const elf_search::ModuleInfo& info) mutable {
        profiler::Module& mod = modules.emplace_back();
        mod.module_id = count++;
        mod.module_name = info.name;
        mod.vaddr = info.vaddr;
        std::transform(info.build_id.begin(), info.build_id.end(), std::back_inserter(mod.build_id),
                       [](const uint8_t byte) { return std::byte{byte}; });

        for (const auto& phdr : info.phdrs) {
          if (phdr.p_type != PT_LOAD) {
            continue;
          }
          mod.loads.push_back({phdr.p_vaddr, phdr.p_memsz, phdr.p_flags});
        }
      });
  if (search_result != ZX_OK) {
    return zx::error(search_result);
  }
  return zx::ok(std::move(modules));
}
