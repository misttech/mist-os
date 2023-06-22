// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// N.B. The offline symbolizer (scripts/symbolize) reads our output,
// don't break it.

#include <elf-search.h>
#include <inttypes.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdint>
#include <iterator>
#include <vector>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/string.h>
#include <fbl/string_printf.h>
#include <inspector/inspector.h>

#include "src/lib/unwinder/fuchsia.h"
#include "src/lib/unwinder/unwind.h"

__EXPORT void inspector_print_backtrace_markup(FILE* f, zx_handle_t process, zx_handle_t thread) {
  static constexpr int kBacktraceFrameLimit = 50;

  // Setup memory and modules.
  unwinder::FuchsiaMemory memory(process);
  std::vector<uint64_t> modules;
  elf_search::ForEachModule(
      *zx::unowned_process{process},
      [&modules](const elf_search::ModuleInfo& info) { modules.push_back(info.vaddr); });

  // Setup registers.
  zx_thread_state_general_regs_t regs;
  if (inspector_read_general_regs(thread, &regs) != ZX_OK) {
    return;
  }
  auto registers = unwinder::FromFuchsiaRegisters(regs);

  auto frames = unwinder::Unwind(&memory, modules, registers, kBacktraceFrameLimit);

  // Print frames.
  int n = 0;
  for (auto& frame : frames) {
    uint64_t pc = 0;
    frame.regs.GetPC(pc);  // won't fail.
    std::string source = "from ";
    switch (frame.trust) {
      case unwinder::Frame::Trust::kScan:
        source += "scan";
        break;
      case unwinder::Frame::Trust::kSCS:
        source += "SCS";
        break;
      case unwinder::Frame::Trust::kPLT:
        source += "PLT";
        break;
      case unwinder::Frame::Trust::kFP:
        source += "FP";
        break;
      case unwinder::Frame::Trust::kCFI:
        source += "CFI";
        break;
      case unwinder::Frame::Trust::kContext:
        source += "context";
        break;
    }
    if (frame.fatal_error) {
      source += "\nunwinding aborted: " + frame.error.msg();
    }
    fprintf(f, "{{{bt:%u:%#" PRIxPTR ":%s}}} %s\n", n, pc, frame.pc_is_return_address ? "ra" : "pc",
            source.c_str());
    n++;
  }

  if (n >= kBacktraceFrameLimit) {
    fprintf(f, "warning: backtrace frame limit exceeded; backtrace may be truncated\n");
  }
}

__EXPORT void inspector_print_markup_context(FILE* f, zx_handle_t process) {
  // We should dump all modules instead of only used ones. See fxbug.dev/125728.
  fprintf(f, "{{{reset}}}\n");
  elf_search::ForEachModule(
      *zx::unowned_process{process}, [f, count = 0u](const elf_search::ModuleInfo& info) mutable {
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
