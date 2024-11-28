// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/driver-buffer-id.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>

#include <cstdint>
#include <limits>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"

namespace display {

namespace {

constexpr DriverBufferCollectionId kOne(1);
constexpr DriverBufferCollectionId kTwo(2);

constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
constexpr DriverBufferCollectionId kLargeId(kLargeIdValue);

TEST(DriverBufferIdTest, DriverBufferIdToFidlDriverBufferId) {
  constexpr DriverBufferId kDriverBufferCollectionOneIndexZero = {
      .buffer_collection_id = kOne,
      .buffer_index = 0,
  };
  constexpr fuchsia_hardware_display_engine::wire::BufferId
      kActualFidlDriverBufferCollectionOneIndexZero =
          ToFidlDriverBufferId(kDriverBufferCollectionOneIndexZero);
  constexpr fuchsia_hardware_display_engine::wire::BufferId
      kExpectedFidlDriverBufferCollectionOneIndexZero = {
          .buffer_collection_id =
              {
                  .value = 1,
              },
          .buffer_index = 0,
      };
  EXPECT_EQ(kActualFidlDriverBufferCollectionOneIndexZero.buffer_collection_id.value,
            kExpectedFidlDriverBufferCollectionOneIndexZero.buffer_collection_id.value);
  EXPECT_EQ(kActualFidlDriverBufferCollectionOneIndexZero.buffer_index,
            kExpectedFidlDriverBufferCollectionOneIndexZero.buffer_index);

  constexpr DriverBufferId kDriverBufferCollectionTwoIndexOne = {
      .buffer_collection_id = kTwo,
      .buffer_index = 1,
  };
  constexpr fuchsia_hardware_display_engine::wire::BufferId
      kActualFidlDriverBufferCollectionTwoIndexOne =
          ToFidlDriverBufferId(kDriverBufferCollectionTwoIndexOne);
  constexpr fuchsia_hardware_display_engine::wire::BufferId
      kExpectedFidlDriverBufferCollectionTwoIndexOne = {
          .buffer_collection_id =
              {
                  .value = 2,
              },
          .buffer_index = 1,
      };
  EXPECT_EQ(kActualFidlDriverBufferCollectionTwoIndexOne.buffer_collection_id.value,
            kExpectedFidlDriverBufferCollectionTwoIndexOne.buffer_collection_id.value);
  EXPECT_EQ(kActualFidlDriverBufferCollectionTwoIndexOne.buffer_index,
            kExpectedFidlDriverBufferCollectionTwoIndexOne.buffer_index);

  constexpr DriverBufferId kDriverBufferCollectionLargeIndexMaxUint = {
      .buffer_collection_id = kLargeId,
      .buffer_index = std::numeric_limits<uint32_t>::max(),
  };
  constexpr fuchsia_hardware_display_engine::wire::BufferId
      kActualFidlDriverBufferCollectionLargeIndexMaxInt =
          ToFidlDriverBufferId(kDriverBufferCollectionLargeIndexMaxUint);
  constexpr fuchsia_hardware_display_engine::wire::BufferId
      kExpectedFidlDriverBufferCollectionLargeIndexMaxInt = {
          .buffer_collection_id =
              {
                  .value = kLargeIdValue,
              },
          .buffer_index = std::numeric_limits<uint32_t>::max(),
      };
  EXPECT_EQ(kActualFidlDriverBufferCollectionLargeIndexMaxInt.buffer_collection_id.value,
            kExpectedFidlDriverBufferCollectionLargeIndexMaxInt.buffer_collection_id.value);
  EXPECT_EQ(kActualFidlDriverBufferCollectionLargeIndexMaxInt.buffer_index,
            kExpectedFidlDriverBufferCollectionLargeIndexMaxInt.buffer_index);
}

TEST(DriverBufferIdTest, FidlDriverBufferIdToDriverBufferId) {
  constexpr fuchsia_hardware_display_engine::wire::BufferId
      kFidlDriverBufferCollectionOneIndexZero = {
          .buffer_collection_id =
              {
                  .value = 1,
              },
          .buffer_index = 0,
      };
  constexpr DriverBufferId kActualDriverBufferCollectionOneIndexZero =
      ToDriverBufferId(kFidlDriverBufferCollectionOneIndexZero);
  constexpr DriverBufferId kExpectedDriverBufferCollectionOneIndexZero = {
      .buffer_collection_id = kOne,
      .buffer_index = 0,
  };
  EXPECT_EQ(kActualDriverBufferCollectionOneIndexZero.buffer_collection_id,
            kExpectedDriverBufferCollectionOneIndexZero.buffer_collection_id);
  EXPECT_EQ(kActualDriverBufferCollectionOneIndexZero.buffer_index,
            kExpectedDriverBufferCollectionOneIndexZero.buffer_index);

  constexpr fuchsia_hardware_display_engine::wire::BufferId kFidlDriverBufferCollectionTwoIndexOne =
      {
          .buffer_collection_id =
              {
                  .value = 2,
              },
          .buffer_index = 1,
      };
  constexpr DriverBufferId kActualDriverBufferCollectionTwoIndexOne =
      ToDriverBufferId(kFidlDriverBufferCollectionTwoIndexOne);
  constexpr DriverBufferId kExpectedDriverBufferCollectionTwoIndexOne = {
      .buffer_collection_id = kTwo,
      .buffer_index = 1,
  };
  EXPECT_EQ(kActualDriverBufferCollectionTwoIndexOne.buffer_collection_id,
            kExpectedDriverBufferCollectionTwoIndexOne.buffer_collection_id);
  EXPECT_EQ(kActualDriverBufferCollectionTwoIndexOne.buffer_index,
            kExpectedDriverBufferCollectionTwoIndexOne.buffer_index);

  constexpr fuchsia_hardware_display_engine::wire::BufferId
      kFidlDriverBufferCollectionLargeIndexUintMax = {
          .buffer_collection_id =
              {
                  .value = kLargeIdValue,
              },
          .buffer_index = std::numeric_limits<uint32_t>::max(),
      };
  constexpr DriverBufferId kActualDriverBufferCollectionLargeIndexUintMax =
      ToDriverBufferId(kFidlDriverBufferCollectionLargeIndexUintMax);
  constexpr DriverBufferId kExpectedDriverBufferCollectionLargeIndexUintMax = {
      .buffer_collection_id = kLargeId,
      .buffer_index = std::numeric_limits<uint32_t>::max(),
  };
  EXPECT_EQ(kActualDriverBufferCollectionLargeIndexUintMax.buffer_collection_id,
            kExpectedDriverBufferCollectionLargeIndexUintMax.buffer_collection_id);
  EXPECT_EQ(kActualDriverBufferCollectionLargeIndexUintMax.buffer_index,
            kExpectedDriverBufferCollectionLargeIndexUintMax.buffer_index);
}

}  // namespace

}  // namespace display
