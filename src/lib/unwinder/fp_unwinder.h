// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_FP_UNWINDER_H_
#define SRC_LIB_UNWINDER_FP_UNWINDER_H_

#include <vector>

#include "src/lib/unwinder/cfi_unwinder.h"
#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/memory.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

// Unwind from the frame pointer. There's no reliable way to detect whether
// a function has frame pointer enabled, so we try our best.
class FramePointerUnwinder {
 public:
  // We need |CfiUnwinder::IsValidPC|.
  explicit FramePointerUnwinder(CfiUnwinder* cfi_unwinder) : cfi_unwinder_(cfi_unwinder) {}

  Error Step(Memory* stack, const Registers& current, Registers& next);

 private:
  CfiUnwinder* cfi_unwinder_;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_FP_UNWINDER_H_
