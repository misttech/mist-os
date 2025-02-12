// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/log-zircon.h>
#include <lib/zircon-internal/unique-backtrace.h>
#include <zircon/assert.h>
#include <zircon/sanitizer.h>

#include <__verbose_abort>
#include <algorithm>
#include <cassert>
#include <cstdarg>
#include <cstddef>
#include <new>

#include "dynlink.h"
#include "stdio/printf_core/wrapper.h"

namespace {

// The code here needs to run before all normal constructors have run, and keep
// working after all normal destructors have run.  The ld::Log type is entirely
// constinit-compatible.  But it needs a destructor, so even a constinit global
// induces a static constructor to register the destructor.  Or else it might
// induce a direct `.fini_array` entry as a static registration of the
// destructor.  In either case, it's hard or impossible to ensure that this
// destructor will run only after all others.  It's important not to tear this
// down until there is no code (including any plain compiled code that's
// instrumented to possibly call into a sanitizer runtime) that might ever call
// into __sanitizer_log_write (or into _dl_log_write otherwise, e.g. via
// dlopen).  So instead of a simple gLog global object here, that object is
// created explicitly via placement new in _dl_log_write_preinit.  In the
// current implementation, the destructor is never run, since it would just be
// closing handles right before process exit and that accomplishes nothing.
alignas(alignof(ld::Log)) std::byte gLogStorage[sizeof(ld::Log)];

// This should get compiled away, but it doesn't hurt much if not, and it makes
// the uses below more natural.
ld::Log& gLog = *reinterpret_cast<ld::Log*>(gLogStorage);

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
void std::__libcpp_verbose_abort(const char* format, ...) noexcept {
  va_list args;
  va_start(args, format);
  Panic(format, args);
  va_end(args);
}

void _dl_log_write(const char* buffer, size_t len) { LogWrite({buffer, len}); }

// This is called by __dls3 before any other function here could be called.
// It's doing a touch of redundant work, since the constructor is only storing
// zeros, and gLogStorage is already implicitly zero-initialized.  But the
// compiler doesn't know this is the first thing to touch gLogStorage.  It's
// only a single word write, so it costs less than just the function call
// overhead, really.
void _dl_log_write_preinit() {
  [[maybe_unused]] ld::Log* log = new (gLogStorage) ld::Log;
  assert(log == &gLog);
}

// This is called by __dls3 for any PA_FD handle in the bootstrap message.
void _dl_log_write_init(zx_handle_t handle, uint32_t info) {
  zx::handle fd{handle};
  if (ld::Log::IsProcessArgsLogFd(info)) {
    gLog.TakeLogFd(std::move(fd));
  }
}

// This is called after the last chance to call _dl_log_write_init.
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
