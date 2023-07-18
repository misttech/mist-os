// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "src/firmware/lib/fastboot/rust/ffi_c/bindings.h"

namespace {

// For now we're just testing that the C/Rust FFI works and we can call a
// function.
TEST(RustTest, Foo) { ASSERT_EQ(foo(), 42); }

}  // namespace
