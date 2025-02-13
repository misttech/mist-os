// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_CFI_UNWINDER_H_
#define SRC_LIB_UNWINDER_CFI_UNWINDER_H_

#include <cstdint>
#include <map>
#include <memory>
#include <utility>
#include <vector>

#include "src/lib/unwinder/cfi_module.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/module.h"
#include "src/lib/unwinder/registers.h"
#include "src/lib/unwinder/unwinder_base.h"

namespace unwinder {

class CfiUnwinder : public UnwinderBase {
 public:
  explicit CfiUnwinder(const std::vector<Module>& modules);

  Error Step(Memory* stack, const Frame& current, Frame& next) override;

  // For other unwinders that want to check whether a value looks like a valid PC.
  bool IsValidPC(uint64_t pc);

  Error GetCfiModuleFor(uint64_t pc, CfiModule** out);

 private:
  // |is_return_address| indicates whether the current PC is pointing to a return address,
  // in which case it'll be adjusted to find the correct CFI entry.
  Error Step(Memory* stack, const Registers& current, Registers& next, bool is_return_address);

  // Mapping from module load addresses to a pair of (module description, lazily-initialized CFI).
  std::map<uint64_t, std::pair<Module, std::unique_ptr<CfiModule>>> module_map_;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_CFI_UNWINDER_H_
