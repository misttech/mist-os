// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/counter.h>

#include <zxtest/zxtest.h>

namespace {

TEST(CounterTest, CreateNotSupported) {
  zx::counter counter;
  ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, zx::counter::create(0u, &counter));
}

}  // namespace
