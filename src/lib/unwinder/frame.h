// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_UNWINDER_FRAME_H_
#define SRC_LIB_UNWINDER_FRAME_H_

#include <cstring>

#include "src/lib/unwinder/error.h"
#include "src/lib/unwinder/registers.h"

namespace unwinder {

struct Frame {
  enum class Trust {
    kScan,     // From scanning the stack with heuristics, least reliable.
    kSCS,      // From the shadow call stack.
    kFP,       // From the frame pointer.
    kPLT,      // From PLT unwinder.
    kCFI,      // From call frame info / .eh_frame section.
    kContext,  // From the input / context, most reliable.
  };

  // Register status at each return site. Unknown registers may be included.
  Registers regs;

  // Whether the PC in |regs| is a return address or a precise code location.
  //
  // PC is usually a precise location for the first frame and a return address for the rest frames.
  // However, it could also be a precise location when it's not a regular function call, e.g., in
  // signal frames or when unwinding into restricted mode in Starnix.
  bool pc_is_return_address;

  // Trust level of the frame.
  Trust trust;

  // Error when unwinding from this frame, which could be non-fatal and present in any frames.
  Error error = Success();

  // Whether the above error is fatal and aborts the unwinding, causing the stack to be incomplete.
  // This could only be true for the last frame.
  bool fatal_error = false;

  // Disallow default constructors.
  Frame(Registers regs, bool pc_is_return_address, Trust trust)
      : regs(std::move(regs)), pc_is_return_address(pc_is_return_address), trust(trust) {}

  // Useful for debugging.
  std::string Describe() const;
};

}  // namespace unwinder

#endif  // SRC_LIB_UNWINDER_FRAME_H_
