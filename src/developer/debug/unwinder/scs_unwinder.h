// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_UNWINDER_SCS_UNWINDER_H_
#define SRC_DEVELOPER_DEBUG_UNWINDER_SCS_UNWINDER_H_

#include "src/developer/debug/unwinder/dwarf_cfi.h"
#include "src/developer/debug/unwinder/memory.h"
#include "src/developer/debug/unwinder/registers.h"

namespace unwinder {

// Unwind from Shadow Call Stacks, only available on arm64.
//
// It's inherently unreliable to unwind from a shadow call stack, because
//  1) The shadow call stack provides nothing other than return addresses.
//  2) A function can choose not to implement shadow call stack, e.g. a leaf
//     function or a library compiled without SCS, and the unwinder has no way
//     to detect; those frames will be dropped silently.
class ShadowCallStackUnwinder {
 public:
  explicit ShadowCallStackUnwinder(Memory* scs) : scs_(scs) {}

  Error Step(const Registers& current, Registers& next);

 private:
  Memory* scs_;
};

}  // namespace unwinder

#endif  // SRC_DEVELOPER_DEBUG_UNWINDER_SCS_UNWINDER_H_
