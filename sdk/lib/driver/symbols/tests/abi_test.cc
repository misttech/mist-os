// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/symbols/symbols.h>

#include <gtest/gtest.h>

TEST(DriverSymbolsAbi, EncodedFidlMessage) {
  static_assert(sizeof(EncodedFidlMessage) == 24);
  static_assert(offsetof(EncodedFidlMessage, bytes) == 0);
  static_assert(offsetof(EncodedFidlMessage, handles) == 8);
  static_assert(offsetof(EncodedFidlMessage, num_bytes) == 16);
  static_assert(offsetof(EncodedFidlMessage, num_handles) == 20);
}
