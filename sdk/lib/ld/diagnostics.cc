// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "diagnostics.h"

#include <cstdarg>

namespace ld {

void DiagnosticsReport::Printf(const char* format, ...) const {
  va_list args;
  va_start(args, format);
  Printf(format, args);
  va_end(args);
}

void CheckErrors(Diagnostics& diag) {
  if (diag.errors() == 0) [[likely]] {
    return;
  }

  diag.report()("startup dynamic linking failed with", diag.errors(), " errors and",
                diag.warnings(), " warnings");
  __builtin_trap();
}

}  // namespace ld
