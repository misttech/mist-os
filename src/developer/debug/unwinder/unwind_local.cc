// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/unwinder/unwind_local.h"

#include <link.h>

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <vector>

#include "src/developer/debug/unwinder/memory.h"
#include "src/developer/debug/unwinder/third_party/libunwindstack/context.h"

namespace unwinder {

std::vector<Frame> UnwindLocal() {
  std::vector<uint64_t> modules;

  // Find all the modules in the current process.
  using CallbackType = int (*)(dl_phdr_info* info, size_t size, void* modules);
  CallbackType dl_iterate_phdr_callback = [](dl_phdr_info* info, size_t, void* p_modules) {
    auto modules = reinterpret_cast<std::vector<uint64_t>*>(p_modules);
    modules->push_back(info->dlpi_addr);
    return 0;
  };
  dl_iterate_phdr(dl_iterate_phdr_callback, &modules);

  LocalMemory mem;
  auto frames = Unwind(&mem, modules, GetContext());

  if (frames.empty()) {
    return {};
  }
  // Drop the first frame.
  return {frames.begin() + 1, frames.end()};
}

}  // namespace unwinder
