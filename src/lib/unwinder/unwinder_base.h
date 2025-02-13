// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_UNWINDER_BASE_H_
#define SRC_LIB_UNWINDER_UNWINDER_BASE_H_

#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/frame.h"

namespace unwinder {

class AsyncMemory;
class CfiUnwinder;
class Memory;

// Base class for all unwinders. The CfiUnwinder derived class offers some utilities for inspecting
// the underlying module synchronously that are used by other derived classes.
class UnwinderBase {
 public:
  explicit UnwinderBase(CfiUnwinder* cfi_unwinder) : cfi_unwinder_(cfi_unwinder) {}

  // Unwind one frame, populating |next| with the new register values. |next| is invalid if an error
  // is returned.
  virtual Error Step(Memory* stack, const Frame& current, Frame& next) = 0;

 protected:
  CfiUnwinder* cfi_unwinder_;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_UNWINDER_BASE_H_
