// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <stdarg.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>

#include <__verbose_abort>

#include "util.h"

namespace {

[[noreturn]] void vpanic(const char* fmt, va_list ap) {
  // This uses zx_debug_write().  The log handle isn't plumbed to __zx_panic.
  vprintl(zx::debuglog{}, fmt, ap);
  va_end(ap);

  // Because the zx::debuglog{} is default constructed above, vprintl() will
  // use zx_debug_write(), and in that path vprintl() will explicitly output a
  // \n after whatever is written via vprintl() above.  So we don't need/want a
  // printl(zx::debuglog{}, "\n") here (at least for now).  This intentionally
  // differs from other __zx_panic() implementations that have the \n added in
  // __zx_panic().

  __builtin_trap();
}

}  // namespace

extern "C" __NO_RETURN __PRINTFLIKE(1, 2) void __zx_panic(const char* fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  vpanic(fmt, ap);
}

// Inline functions in libc++ headers call this.
[[noreturn]] void std::__libcpp_verbose_abort(const char* format, ...) {
  va_list ap;
  va_start(ap, format);
  vpanic(format, ap);
}
