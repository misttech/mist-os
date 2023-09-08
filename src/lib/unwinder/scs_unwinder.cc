// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/unwinder/scs_unwinder.h"

namespace unwinder {

Error ShadowCallStackUnwinder::Step(Memory* scs, const Registers& current, Registers& next) {
  if (current.arch() != Registers::Arch::kArm64) {
    return Error("Shadow call stack is only supported on arm64");
  }
  uint64_t x18;
  if (auto err = current.Get(RegisterID::kArm64_x18, x18); err.has_err()) {
    return err;
  }
  if (!x18) {
    return Error("x18 is not available");
  }

  // The shadow call stack is pushed/popped via
  //
  //    str     x30, [x18], #8    ; post-indexed
  //    ...
  //    ldr     x30, [x18, #-8]!  ; pre-indexed
  //
  // So x18 points to the next available slots.
  uint64_t ra;
  if (auto err = scs->Read(x18 - 8, ra); err.has_err()) {
    return err;
  }

  // A zero ra indicates the beginning of the shadow call stack.
  if (!ra) {
    return Success();
  }
  if (!cfi_unwinder_->IsValidPC(ra)) {
    return Error("Invalid shadow call stack");
  }

  next.SetPC(ra);
  next.Set(RegisterID::kArm64_x18, x18 - 8);
  return Success();
}

}  // namespace unwinder
