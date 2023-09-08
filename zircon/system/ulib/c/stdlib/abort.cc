// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/stdlib/abort.h"

#include <lib/zircon-internal/unique-backtrace.h>
#include <zircon/syscalls.h>

#include "src/__support/common.h"

namespace LIBC_NAMESPACE {

LLVM_LIBC_FUNCTION(void, abort, ()) {
  for (;;) {
    CRASH_WITH_UNIQUE_BACKTRACE();
    _zx_process_exit(ZX_TASK_RETCODE_EXCEPTION_KILL);
  }
}

}  // namespace LIBC_NAMESPACE
