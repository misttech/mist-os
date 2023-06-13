// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/buffer-collection-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

TEST(BufferCollectionId, Equality) {
  constexpr BufferCollectionId kOne(1);
  constexpr BufferCollectionId kAnotherOne(1);
  constexpr BufferCollectionId kTwo(2);

  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_NE(kOne, kTwo);
}

TEST(BufferCollectionId, FidlConversion) {
  EXPECT_EQ(ToBufferCollectionId(1), BufferCollectionId(1));
  EXPECT_EQ(ToBufferCollectionId(1).value(), uint64_t{1});

  EXPECT_EQ(ToFidlBufferCollectionId(BufferCollectionId(1)), uint64_t{1});

  const uint64_t kLargeBufferCollectionIdValue = uint64_t{1} << 63;
  EXPECT_EQ(ToFidlBufferCollectionId(BufferCollectionId(kLargeBufferCollectionIdValue)),
            kLargeBufferCollectionIdValue);
}

}  // namespace

}  // namespace display
