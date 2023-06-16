// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SYMBOLIZATION_CONTEXT_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SYMBOLIZATION_CONTEXT_H_

#include <lib/zx/process.h>
#include <lib/zx/result.h>

#include <map>
#include <string>
#include <vector>

namespace profiler {
struct Segment {
  uintptr_t p_vaddr;
  uintptr_t p_memsz;
  uint64_t p_flags;
};

struct Module {
  size_t module_id;
  std::string module_name;
  std::string build_id;
  uintptr_t vaddr;
  std::vector<Segment> loads;
};

struct SymbolizationContext {
  std::map<zx_koid_t, std::vector<Module>> process_contexts;
};

zx::result<std::vector<Module>> GetProcessModules(const zx::unowned_process& process);
}  // namespace profiler
#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SYMBOLIZATION_CONTEXT_H_
