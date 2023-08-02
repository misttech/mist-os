// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string.h>
#include <unistd.h>

#include "diagnostics.h"
#include "posix.h"
#include "stdio/printf_core/wrapper.h"

namespace ld {
namespace {

int StderrWrite(std::string_view str) {
  return static_cast<int>(write(STDERR_FILENO, str.data(), str.size()));
}

}  // namespace

constexpr size_t kBufferSize = 128;

// The formatted message from elfldltl::PrintfDiagnosticsReport should be a
// single line with no newline, so append one.  Then just send all the data
// directly to stderr.
void DiagnosticsReport::Printf(const char* format, va_list args) const {
  __llvm_libc::printf_core::Printf<kBufferSize, __llvm_libc::printf_core::PrintfNewline::kYes>(
      StderrWrite, format, args);
}

}  // namespace ld

// TODO(mcgrathr): The llvm-libc implementation would be almost fine, but needs
// to be compiled in a way where it won't try to use a thread_local variable.
// Some special plumbing could achieve that, maybe do it later.
char* strerror(int errnum) {
  static char buffer[32];
  snprintf(buffer, sizeof(buffer), "errno %d", errnum);
  return buffer;
}
