// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "symbolizer_markup.h"

#include <elf.h>
#include <lib/symbolizer-markup/writer.h>

#include "sampler.h"
#include "symbolization_context.h"

struct Sink {
  void operator()(std::string_view s) { value.append(s); }

  std::string& value;
};

std::string profiler::symbolizer_markup::FormatModule(const profiler::Module& mod) {
  const size_t kPageSize = zx_system_get_page_size();
  std::string markup;
  ::symbolizer_markup::Writer writer(Sink{markup});
  writer.ElfModule(mod.module_id, mod.module_name, mod.build_id).Newline();
  for (const profiler::Segment& segment : mod.loads) {
    uintptr_t start = segment.p_vaddr & -kPageSize;
    uintptr_t end = (segment.p_vaddr + segment.p_memsz + kPageSize - 1) & -kPageSize;

    ::symbolizer_markup::MemoryPermissions perms{
        .read = (segment.p_flags & PF_R) != 0,
        .write = (segment.p_flags & PF_W) != 0,
        .execute = (segment.p_flags & PF_X) != 0,
    };
    writer.LoadImageMmap(mod.vaddr + start, end - start, mod.module_id, perms, start).Newline();
  }
  return markup;
}

std::string profiler::symbolizer_markup::FormatSample(const profiler::Sample& sample) {
  std::string markup;
  ::symbolizer_markup::Writer writer(Sink{markup});
  writer.DecimalDigits(sample.pid).Newline().DecimalDigits(sample.tid).Newline();
  for (unsigned n = 0; n < sample.stack.size(); n++) {
    if (n == 0) {
      writer.ExactPcFrame(n, sample.stack[n]).Newline();
    } else {
      writer.ReturnAddressFrame(n, sample.stack[n]).Newline();
    }
  }
  return markup;
}
