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

#include "sdk/lib/fit/include/lib/fit/function.h"
#include "src/lib/unwinder/cfi_module.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/module.h"
#include "src/lib/unwinder/registers.h"
#include "src/lib/unwinder/unwinder_base.h"

namespace unwinder {

// Contains the information relevant for the given Module. The CfiModules are lazily loaded when
// this module is needed.
struct CfiModuleInfo {
  bool IsValidPC(uint64_t pc) const;

  Module module;
  // The loaded binary file corresponding to this module.. This is the (possibly stripped) binary
  // file. It may contain an .eh_frame section, and optionally a .debug_frame section (in the case
  // it is unstripped).
  std::unique_ptr<CfiModule> binary;
  // The loaded debug info file, if present. This is an optional addition to the binary file
  // above. When non-null, this file will be inspected for a .debug_frame section before the
  // binary file. This is only loaded and used if the |debug_info_memory| is non-null in |module|.
  std::unique_ptr<CfiModule> debug_info;
};

class CfiUnwinder : public UnwinderBase {
 public:
  explicit CfiUnwinder(const std::vector<Module>& modules);

  Error Step(Memory* stack, const Frame& current, Frame& next) override;

  void AsyncStep(AsyncMemory* stack, const Frame& current,
                 fit::callback<void(Error, Registers)> cb) override;

  // For other unwinders that want to check whether a value looks like a valid PC.
  bool IsValidPC(uint64_t pc);

  Error GetCfiModuleInfoForPc(uint64_t pc, CfiModuleInfo** out);

 private:
  // |is_return_address| indicates whether the current PC is pointing to a return address,
  // in which case it'll be adjusted to find the correct CFI entry.
  Error Step(Memory* stack, const Registers& current, Registers& next, bool is_return_address);

  void AsyncStep(AsyncMemory* stack, Registers current, bool is_return_address,
                 fit::callback<void(Error, Registers)> cb);

  // Mapping from module load addresses to a pair of (module description, lazily-initialized CFI
  // modules for the binary and optional debugging info).
  std::map<uint64_t, CfiModuleInfo> module_map_;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_CFI_UNWINDER_H_
