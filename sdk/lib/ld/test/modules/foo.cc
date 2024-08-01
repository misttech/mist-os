// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>

// This template is useful for providing a function that calls foo from a
// a dependency.

extern "C" [[gnu::visibility("default")]] int64_t call_foo();

extern "C" int64_t foo();

extern "C" int64_t call_foo() { return foo(); }
