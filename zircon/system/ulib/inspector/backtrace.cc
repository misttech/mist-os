// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// N.B. The offline symbolizer (scripts/symbolize) reads our output,
// don't break it.

#include "zircon/system/ulib/inspector/backtrace.h"

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

#include "dso-list-impl.h"
#include "inspector/inspector.h"
#include "src/lib/unwinder/fuchsia.h"
#include "src/lib/unwinder/unwind.h"
#include "utils-impl.h"

namespace inspector {

constexpr int kBacktraceFrameLimit = 50;

struct Frame {
  uint64_t pc;

  // Extra message that is shown after the frame.
  fbl::String message;
};

static std::vector<Frame> unwind_from_unwinder(zx_handle_t process, zx_handle_t thread) {
  // Setup memory and modules.
  unwinder::FuchsiaMemory memory(process);
  std::vector<uint64_t> modules;
  elf_search::ForEachModule(
      *zx::unowned_process{process},
      [&modules](const elf_search::ModuleInfo& info) { modules.push_back(info.vaddr); });

  // Setup registers.
  zx_thread_state_general_regs_t regs;
  if (inspector_read_general_regs(thread, &regs) != ZX_OK) {
    return {};
  }
  auto registers = unwinder::FromFuchsiaRegisters(regs);

  auto frames = unwinder::Unwind(&memory, modules, registers, kBacktraceFrameLimit);

  // Convert frames.
  std::vector<Frame> res;
  res.reserve(frames.size());
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
    res.push_back({pc, source});
  }
  return res;
}

static void print_stack(FILE* f, const std::vector<Frame>& stack) {
  int n = 0;
  for (auto& frame : stack) {
    const char* address_type = "ra";
    if (n == 0) {
      address_type = "pc";
    }
    fprintf(f, "{{{bt:%u:%#" PRIxPTR ":%s}}} %s\n", n++, frame.pc, address_type,
            frame.message.c_str());
  }
  if (n >= kBacktraceFrameLimit) {
    fprintf(f, "warning: backtrace frame limit exceeded; backtrace may be truncated\n");
  }
}

extern "C" __EXPORT void inspector_print_backtrace_markup(FILE* f, zx_handle_t process,
                                                          zx_handle_t thread) {
  print_backtrace_markup(f, process, thread, /*skip_markup_context=*/true);
}

void print_backtrace_markup(FILE* f, zx_handle_t process, zx_handle_t thread,
                            const bool skip_markup_context) {
  if (!skip_markup_context) {
    // TODO(fxbug.dev/125728): always dump the full module list for now and not just the modules
    // involved in this stack trace, because the context might be re-used by other threads.
    print_markup_context(f, process, {});
  }

  print_stack(f, unwind_from_unwinder(process, thread));
}

}  // namespace inspector
