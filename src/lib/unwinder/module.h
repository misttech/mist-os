// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_MODULE_H_
#define SRC_LIB_UNWINDER_MODULE_H_

#include <cstdint>
#include <cstring>
#include <map>

#include "src/lib/unwinder/memory.h"

namespace unwinder {

// An ELF module.
struct Module {
  // AddressMode determines the layout of ELF structures.
  enum class AddressMode {
    kProcess,  // Mapped in a running process. Data will be read from a vaddr.
    kFile,     // Packed as a file. Data will be read from an offset.
  };

  // The load address.
  uint64_t load_address;

  // Data accessor. Cannot be null.
  Memory* memory;

  // Address mode.
  AddressMode mode;

  Module(uint64_t addr, Memory* mem, AddressMode mod)
      : load_address(addr), memory(mem), mode(mod) {}
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_MODULE_H_
