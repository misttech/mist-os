// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/layout.h>
#include <lib/elfldltl/phdr.h>
#include <lib/stdcompat/span.h>
#include <zircon/sanitizer.h>

#include <cstdint>

#include "dynlink.h"

void _dl_phdr_report_globals(sanitizer_memory_snapshot_callback_t* callback, void* callback_arg,
                             size_t load_bias, const Phdr* phdrs, size_t phnum) {
  using ElfPhdr = elfldltl::Elf<>::Phdr;
  static_assert(sizeof(ElfPhdr) == sizeof(Phdr));

  const cpp20::span<const ElfPhdr> elf_phdrs{
      reinterpret_cast<const ElfPhdr*>(phdrs),
      phnum,
  };

  auto segment_callback = [load_bias, callback, callback_arg](uintptr_t start, size_t size) {
    start += load_bias;
    callback(reinterpret_cast<std::byte*>(start), size, callback_arg);
    return true;
  };

  elfldltl::OnPhdrWritableSegments(elf_phdrs, segment_callback);
}
