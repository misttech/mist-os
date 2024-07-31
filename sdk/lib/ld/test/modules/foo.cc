// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>

// This module is useful for returning the value of a symbol that may be
// resolved by any number of its dependencies.

extern "C" [[gnu::visibility("default")]] int64_t call_foo();

extern "C" int64_t foo();

extern "C" int64_t call_foo() { return foo(); }
