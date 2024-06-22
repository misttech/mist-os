// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/log-zircon.h>
#include <lib/zircon-internal/unique-backtrace.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/sanitizer.h>

#include <__verbose_abort>
#include <algorithm>
#include <cstdarg>

#include "dynlink.h"
#include "stdio/printf_core/wrapper.h"

namespace {

// This is constinit so it can be used before constructors run.  But it still
// engenders a global constructor to register its global destructor.  This is
// fine, since that registration doesn't need to happen before it gets used.
__CONSTINIT ld::Log gLog;

void LogWrite(std::string_view str) {
  if (!gLog) {
    return;
  }

  while (!str.empty()) {
    std::string_view chunk = str.substr(0, std::min(ld::Log::kBufferSize, str.size()));

    // Write only a single line at a time so each line gets tagged.
    if (size_t nl = chunk.find('\n'); nl != std::string_view::npos) {
      chunk = chunk.substr(0, nl + 1);
    }

    if (gLog(chunk) < 0) {
      CRASH_WITH_UNIQUE_BACKTRACE();
    }

    str.remove_prefix(chunk.size());
  }
}

// The panic entry points are only for use within the hermetic_source_set(),
// i.e. the call graph of _dl_log_write and the other methods.  The normal libc
// entry points use the compiler ABI and stdio and so forth.

[[noreturn]] void Panic(const char* format, va_list args) {
  using LIBC_NAMESPACE::printf_core::Printf;
  using LIBC_NAMESPACE::printf_core::PrintfNewline;
  Printf<ld::Log::kBufferSize, PrintfNewline::kYes>(gLog, format, args);
  __builtin_trap();
}

}  // namespace

// <zircon/assert.h> macros use this.
extern "C" void __zx_panic(const char* format, ...) {
  va_list args;
  va_start(args, format);
  Panic(format, args);
  va_end(args);
}

// libc++ header code uses this.
void std::__libcpp_verbose_abort(const char* format, ...) {
  va_list args;
  va_start(args, format);
  Panic(format, args);
  va_end(args);
}

void _dl_log_write(const char* buffer, size_t len) { LogWrite({buffer, len}); }

// This is called by __dls3 for any PA_FD handle in the bootstrap message.
void _dl_log_write_init(zx_handle_t handle, uint32_t info) {
  zx::handle fd{handle};
  if (ld::Log::IsProcessArgsLogFd(info)) {
    gLog.TakeLogFd(std::move(fd));
  }
}

// This is called after the last change to call _dl_log_write_init.
void _dl_log_write_init_fallback() {
  if (gLog) {
    return;
  }

  // TODO(https://fxbug.dev/42107086): For a long time this has always used a
  // kernel log channel when nothing else is passed in the bootstrap message.
  // This still needs to be replaced by a proper unprivileged logging scheme.
  zx::debuglog debuglog;
  zx::debuglog::create(zx::resource{}, 0, &debuglog);
  gLog.set_debuglog(std::move(debuglog));
}
