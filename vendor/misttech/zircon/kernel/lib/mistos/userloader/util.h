// Copyright 2016 The Fuchsia Authors
// Copyright 2024 Mist Tecnologia LTDA
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_UTIL_H_
#define ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_UTIL_H_

#include <lib/mistos/util/status.h>
#include <lib/mistos/zx/debuglog.h>
#include <lib/stdcompat/source_location.h>
#include <stdarg.h>
#include <zircon/types.h>

// printl() is printf-like, understanding %s %p %d %u %x %zu %zd %zx.
// No other formatting features are supported.
[[gnu::format(printf, 2, 3)]] void printl(const zx::debuglog& log, const char* fmt, ...);
void vprintl(const zx::debuglog& log, const char* fmt, va_list ap);

// fail() combines printl() with process exit
[[noreturn, gnu::format(printf, 2, 3)]] void fail(const zx::debuglog& log, const char* fmt, ...);

#define CHECK(log, status, fmt, ...)                                      \
  do {                                                                    \
    if (status != ZX_OK)                                                  \
      fail(log, "%s: " fmt, zx_status_get_string(status), ##__VA_ARGS__); \
  } while (0)

template <typename T>
T DuplicateOrDie(const zx::debuglog& log, const T& typed_handle,
                 cpp20::source_location src_loc = cpp20::source_location::current());

zx::debuglog DuplicateOrDie(const zx::debuglog& log,
                            cpp20::source_location src_loc = cpp20::source_location::current());

#endif  // ZIRCON_KERNEL_LIB_MISTOS_USERLOADER_UTIL_H_
