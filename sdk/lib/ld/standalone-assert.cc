// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <__verbose_abort>

// TODO(fxbug.dev/130483): These should print something useful instead of just crashing.

extern "C" void __assert_fail(const char*, const char*, int, const char*) { __builtin_trap(); }

void std::__libcpp_verbose_abort(const char* format, ...) { __builtin_trap(); }
