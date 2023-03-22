// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_UNWINDER_DWARF_UNWINDER_H_
#define SRC_DEVELOPER_DEBUG_UNWINDER_DWARF_UNWINDER_H_

#include "src/developer/debug/unwinder/dwarf_cfi.h"
#include "src/developer/debug/unwinder/memory.h"
#include "src/developer/debug/unwinder/registers.h"

namespace unwinder {

class DwarfUnwinder {
 public:
  DwarfUnwinder(Memory* stack, std::map<uint64_t, Memory*> module_map)
      : stack_(stack), module_map_(std::move(module_map)) {}

  Error Step(Registers current, Registers& next, bool is_return_address);

 private:
  // Inputs.
  Memory* stack_;
  std::map<uint64_t, Memory*> module_map_;

  // Lazy-initialized CFI of each module.
  std::map<uint64_t, DwarfCfi> cfi_map_;
};

}  // namespace unwinder

#endif  // SRC_DEVELOPER_DEBUG_UNWINDER_DWARF_UNWINDER_H_
