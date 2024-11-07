// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_STDFORMAT_PRINT_H_
#define SRC_LIB_STDFORMAT_PRINT_H_

#include <lib/stdcompat/version.h>

#if defined(__cpp_lib_print) && __cpp_lib_print >= 202207L && !defined(LIB_STDFORMAT_USE_POLYFILLS)

#include <print>

namespace cpp23 {

using std::print;
using std::println;

}  // namespace cpp23

#else

#include <cstdio>
#include <format>

#include "internal/print.h"

namespace cpp23 {

template <typename... Args>
void print(std::format_string<Args...> fmt, Args&&... args) {
  internal::vprint(stdout, fmt.get(), std::make_format_args(args...));
}

template <typename... Args>
void print(std::FILE* stream, std::format_string<Args...> fmt, Args&&... args) {
  internal::vprint(stream, fmt.get(), std::make_format_args(args...));
}

template <typename... Args>
void println(std::FILE* stream, std::format_string<Args...> fmt, Args&&... args) {
  internal::vprint(stream, fmt.get(), std::make_format_args(args...));
  fputc('\n', stream);
}

template <typename... Args>
void println(std::format_string<Args...> fmt, Args&&... args) {
  println(stdout, fmt, std::forward<Args>(args)...);
}

}  // namespace cpp23

#endif  // #if __cplusplus >= 202207L && !defined(LIB_STDFORMAT_USE_POLYFILLS)

#endif  // SRC_LIB_STDFORMAT_PRINT_H_
