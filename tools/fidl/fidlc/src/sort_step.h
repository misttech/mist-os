// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_SRC_SORT_STEP_H_
#define TOOLS_FIDL_FIDLC_SRC_SORT_STEP_H_

#include "tools/fidl/fidlc/src/compiler.h"

namespace fidlc {

// SortStep topologically sorts the library's decls, storing the result in
// library()->declaration_order. It assumes there are no cycles, since the
// CompileStep should detect them and fail with ErrIncludeCycle.
class SortStep : public Compiler::Step {
 public:
  using Step::Step;

 private:
  void RunImpl() override;
};

}  // namespace fidlc

#endif  // TOOLS_FIDL_FIDLC_SRC_SORT_STEP_H_
