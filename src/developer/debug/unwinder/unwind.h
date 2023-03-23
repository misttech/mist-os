// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_UNWINDER_UNWIND_H_
#define SRC_DEVELOPER_DEBUG_UNWINDER_UNWIND_H_

#include <cstdint>
#include <utility>
#include <vector>

#include "src/developer/debug/unwinder/dwarf_unwinder.h"
#include "src/developer/debug/unwinder/memory.h"
#include "src/developer/debug/unwinder/module.h"
#include "src/developer/debug/unwinder/registers.h"

namespace unwinder {

struct Frame {
  enum class Trust {
    kScan,     // From scanning the stack with heuristics, least reliable.
    kFP,       // From the frame pointer.
    kSCS,      // From the shadow call stack.
    kCFI,      // From call frame info / .eh_frame section.
    kContext,  // From the input / context, most reliable.
  };

  // Register status at each return site. Only known values will be included.
  Registers regs;

  // Trust level of the frame.
  Trust trust;

  // Error when unwinding from this frame, which should be empty except for the
  // last frame.
  Error error;

  // Disallow default constructors.
  Frame(Registers regs, Trust trust, Error error)
      : regs(std::move(regs)), trust(trust), error(std::move(error)) {}

  // Useful for debugging.
  std::string Describe() const;
};

// The main unwinder. It caches the unwind tables so repeated unwinding is faster.
//
// The API is designed to be flexible so that it can support both online unwinding from a process'
// memory, and offline unwinding from stack snapshots with ELF files on disk.
class Unwinder {
 public:
  // Initialize the unwinder from a vector of modules.
  //
  // Each module can supply its own data accessor and address mode.
  explicit Unwinder(const std::vector<Module>& modules);

  // Unwind from a stack and a set of registers.
  //
  // |stack| could be null, in which case it will become an |UnavailableMemory|.
  std::vector<Frame> Unwind(Memory* stack, const Registers& registers, size_t max_depth = 50);

 private:
  DwarfUnwinder dwarf_unwinder_;
};

// Unwind with given memory, modules, and registers.
//
// This provides an simplified API than the above Unwinder class but comes without a cache.
// The modules are provided as base addresses and are accessed through the memory.
std::vector<Frame> Unwind(Memory* memory, const std::vector<uint64_t>& modules,
                          const Registers& registers, size_t max_depth = 50);

}  // namespace unwinder

#endif  // SRC_DEVELOPER_DEBUG_UNWINDER_UNWIND_H_
