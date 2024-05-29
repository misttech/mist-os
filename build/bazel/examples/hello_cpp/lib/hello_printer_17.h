// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef BUILD_BAZEL_EXAMPLES_HELLO_CPP_LIB_HELLO_PRINTER_17_H_
#define BUILD_BAZEL_EXAMPLES_HELLO_CPP_LIB_HELLO_PRINTER_17_H_

// In real code, use the macros in <zircon/availability.h> for such checks.
// This is an exceptional case where a check for equality is needed.
#if __Fuchsia_API_level__ == 17

namespace hello_printer_17 {

// An example printer which should only be included in API level 17. This is only
// used to test our bazel select conditions.
class HelloPrinter {
 public:
  HelloPrinter() {}
  void PrintHello();
};

}  // namespace hello_printer_17

#endif  // __Fuchsia_API_level__ == 17

#endif  // BUILD_BAZEL_EXAMPLES_HELLO_CPP_LIB_HELLO_PRINTER_17_H_
