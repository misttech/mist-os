// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_CFI_UNWINDER_H_
#define SRC_LIB_UNWINDER_CFI_UNWINDER_H_

#include "src/lib/unwinder/cfi_module.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/module.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

class CfiUnwinder {
 public:
  explicit CfiUnwinder(const std::vector<Module>& modules);

  // |is_return_address| indicates whether the current PC is pointing to a return address,
  // in which case it'll be adjusted to find the correct CFI entry.
  Error Step(Memory* stack, Registers current, Registers& next, bool is_return_address);

  // For other unwinders that want to check whether a value looks like a valid PC.
  bool IsValidPC(uint64_t pc);

 private:
  Error GetCfiModuleFor(uint64_t pc, CfiModule** out);

  // Inputs.
  std::map<uint64_t, Module> module_map_;

  // Lazy-initialized CFI of each module.
  std::map<uint64_t, CfiModule> cfi_map_;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_CFI_UNWINDER_H_
