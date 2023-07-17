// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "diagnostics.h"

namespace ld {

void CheckErrors(Diagnostics& diag) {
  if (diag.errors() == 0) [[likely]] {
    return;
  }

  diag.report()("startup dynamic linking failed with", diag.errors(), "errors and", diag.warnings(),
                "warnings");
  __builtin_trap();
}

}  // namespace ld
