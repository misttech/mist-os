// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_SCS_UNWINDER_H_
#define SRC_LIB_UNWINDER_SCS_UNWINDER_H_

#include "src/lib/unwinder/cfi_unwinder.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

// Unwind from Shadow Call Stacks, only available on arm64.
//
// It's inherently unreliable to unwind from a shadow call stack, because
//  1) The shadow call stack provides nothing other than return addresses, so it's not possible to
//     unwind the following frames with other unwinders.
//  2) A function can choose not to implement shadow call stack, e.g. a library compiled without
//     SCS, and the unwinder has no way to detect; those frames will be dropped silently.
class ShadowCallStackUnwinder {
 public:
  // We need |CfiUnwinder::IsValidPC|.
  explicit ShadowCallStackUnwinder(CfiUnwinder* cfi_unwinder) : cfi_unwinder_(cfi_unwinder) {}

  Error Step(Memory* scs, const Registers& current, Registers& next);

 private:
  CfiUnwinder* cfi_unwinder_;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_SCS_UNWINDER_H_
