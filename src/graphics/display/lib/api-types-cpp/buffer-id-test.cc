// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/buffer-id.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>
#include <limits>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types-cpp/buffer-collection-id.h"

namespace display {

namespace {

constexpr BufferCollectionId kOne(1);
constexpr BufferCollectionId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr BufferCollectionId kLargeId(kLargeIdValue);

TEST(BufferIdTest, BufferIdToFidlBufferId) {
  constexpr BufferId kBufferCollectionOneIndexZero = {
      .buffer_collection_id = kOne,
      .buffer_index = 0,
  };
  constexpr fuchsia_hardware_display::wire::BufferId kActualFidlBufferCollectionOneIndexZero =
      ToFidlBufferId(kBufferCollectionOneIndexZero);
  constexpr fuchsia_hardware_display::wire::BufferId kExpectedFidlBufferCollectionOneIndexZero = {
      .buffer_collection_id =
          {
              .value = 1,
          },
      .buffer_index = 0,
  };
  EXPECT_EQ(kActualFidlBufferCollectionOneIndexZero.buffer_collection_id.value,
            kExpectedFidlBufferCollectionOneIndexZero.buffer_collection_id.value);
  EXPECT_EQ(kActualFidlBufferCollectionOneIndexZero.buffer_index,
            kExpectedFidlBufferCollectionOneIndexZero.buffer_index);

  constexpr BufferId kBufferCollectionTwoIndexOne = {
      .buffer_collection_id = kTwo,
      .buffer_index = 1,
  };
  constexpr fuchsia_hardware_display::wire::BufferId kActualFidlBufferCollectionTwoIndexOne =
      ToFidlBufferId(kBufferCollectionTwoIndexOne);
  constexpr fuchsia_hardware_display::wire::BufferId kExpectedFidlBufferCollectionTwoIndexOne = {
      .buffer_collection_id =
          {
              .value = 2,
          },
      .buffer_index = 1,
  };
  EXPECT_EQ(kActualFidlBufferCollectionTwoIndexOne.buffer_collection_id.value,
            kExpectedFidlBufferCollectionTwoIndexOne.buffer_collection_id.value);
  EXPECT_EQ(kActualFidlBufferCollectionTwoIndexOne.buffer_index,
            kExpectedFidlBufferCollectionTwoIndexOne.buffer_index);

  constexpr BufferId kBufferCollectionLargeIndexMaxUint = {
      .buffer_collection_id = kLargeId,
      .buffer_index = std::numeric_limits<uint32_t>::max(),
  };
  constexpr fuchsia_hardware_display::wire::BufferId kActualFidlBufferCollectionLargeIndexMaxInt =
      ToFidlBufferId(kBufferCollectionLargeIndexMaxUint);
  constexpr fuchsia_hardware_display::wire::BufferId kExpectedFidlBufferCollectionLargeIndexMaxInt =
      {
          .buffer_collection_id =
              {
                  .value = kLargeIdValue,
              },
          .buffer_index = std::numeric_limits<uint32_t>::max(),
      };
  EXPECT_EQ(kActualFidlBufferCollectionLargeIndexMaxInt.buffer_collection_id.value,
            kExpectedFidlBufferCollectionLargeIndexMaxInt.buffer_collection_id.value);
  EXPECT_EQ(kActualFidlBufferCollectionLargeIndexMaxInt.buffer_index,
            kExpectedFidlBufferCollectionLargeIndexMaxInt.buffer_index);
}

TEST(BufferIdTest, FidlBufferIdToBufferId) {
  constexpr fuchsia_hardware_display::wire::BufferId kFidlBufferCollectionOneIndexZero = {
      .buffer_collection_id =
          {
              .value = 1,
          },
      .buffer_index = 0,
  };
  constexpr BufferId kActualBufferCollectionOneIndexZero =
      ToBufferId(kFidlBufferCollectionOneIndexZero);
  constexpr BufferId kExpectedBufferCollectionOneIndexZero = {
      .buffer_collection_id = kOne,
      .buffer_index = 0,
  };
  EXPECT_EQ(kActualBufferCollectionOneIndexZero.buffer_collection_id,
            kExpectedBufferCollectionOneIndexZero.buffer_collection_id);
  EXPECT_EQ(kActualBufferCollectionOneIndexZero.buffer_index,
            kExpectedBufferCollectionOneIndexZero.buffer_index);

  constexpr fuchsia_hardware_display::wire::BufferId kFidlBufferCollectionTwoIndexOne = {
      .buffer_collection_id =
          {
              .value = 2,
          },
      .buffer_index = 1,
  };
  constexpr BufferId kActualBufferCollectionTwoIndexOne =
      ToBufferId(kFidlBufferCollectionTwoIndexOne);
  constexpr BufferId kExpectedBufferCollectionTwoIndexOne = {
      .buffer_collection_id = kTwo,
      .buffer_index = 1,
  };
  EXPECT_EQ(kActualBufferCollectionTwoIndexOne.buffer_collection_id,
            kExpectedBufferCollectionTwoIndexOne.buffer_collection_id);
  EXPECT_EQ(kActualBufferCollectionTwoIndexOne.buffer_index,
            kExpectedBufferCollectionTwoIndexOne.buffer_index);

  constexpr fuchsia_hardware_display::wire::BufferId kFidlBufferCollectionLargeIndexUintMax = {
      .buffer_collection_id =
          {
              .value = kLargeIdValue,
          },
      .buffer_index = std::numeric_limits<uint32_t>::max(),
  };
  constexpr BufferId kActualBufferCollectionLargeIndexUintMax =
      ToBufferId(kFidlBufferCollectionLargeIndexUintMax);
  constexpr BufferId kExpectedBufferCollectionLargeIndexUintMax = {
      .buffer_collection_id = kLargeId,
      .buffer_index = std::numeric_limits<uint32_t>::max(),
  };
  EXPECT_EQ(kActualBufferCollectionLargeIndexUintMax.buffer_collection_id,
            kExpectedBufferCollectionLargeIndexUintMax.buffer_collection_id);
  EXPECT_EQ(kActualBufferCollectionLargeIndexUintMax.buffer_index,
            kExpectedBufferCollectionLargeIndexUintMax.buffer_index);
}

}  // namespace

}  // namespace display
