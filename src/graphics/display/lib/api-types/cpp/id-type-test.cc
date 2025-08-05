// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/id-type.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>
#include <functional>
#include <map>
#include <unordered_map>

#include <gtest/gtest.h>

namespace display::internal {

namespace {

class IdTypeWithBanjoDefaultTraitsTest : public ::testing::Test {
 protected:
  using TestDisplayIdTraits =
      DefaultIdTypeTraits<uint64_t, fuchsia_hardware_display_types::wire::DisplayId, uint64_t>;
  using TestDisplayId = IdType<TestDisplayIdTraits>;

  static constexpr TestDisplayId kOne = TestDisplayId(1);
  static constexpr TestDisplayId kAnotherOne = TestDisplayId(1);
  static constexpr TestDisplayId kTwo = TestDisplayId(2);

  static constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
  static constexpr TestDisplayId kLargeId = TestDisplayId(kLargeIdValue);
};

TEST_F(IdTypeWithBanjoDefaultTraitsTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, OrderingForEqualValues) {
  EXPECT_FALSE(kOne < kAnotherOne);
  EXPECT_FALSE(kAnotherOne < kOne);

  EXPECT_LE(kOne, kAnotherOne);
  EXPECT_LE(kAnotherOne, kOne);

  EXPECT_FALSE(kOne > kAnotherOne);
  EXPECT_FALSE(kAnotherOne > kOne);

  EXPECT_GE(kOne, kAnotherOne);
  EXPECT_GE(kAnotherOne, kAnotherOne);
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, OrderingForDifferentValues) {
  EXPECT_LT(kOne, kTwo);
  EXPECT_FALSE(kTwo < kOne);

  EXPECT_LE(kOne, kTwo);
  EXPECT_FALSE(kTwo <= kOne);

  EXPECT_FALSE(kOne > kTwo);
  EXPECT_GT(kTwo, kOne);

  EXPECT_FALSE(kOne >= kTwo);
  EXPECT_GE(kTwo, kOne);
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, HashSpecialization) {
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{1}), std::hash<TestDisplayId>()(kOne));
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{2}), std::hash<TestDisplayId>()(kTwo));
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{kLargeIdValue}), std::hash<TestDisplayId>()(kLargeId));
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, OrderedMapKeyUsage) {
  std::map<TestDisplayId, int> ordered_map;
  ordered_map[kOne] = 1;
  ordered_map[kTwo] = 2;

  EXPECT_EQ(1, ordered_map[kOne]);
  EXPECT_EQ(2, ordered_map[kTwo]);
  EXPECT_EQ(0u, ordered_map.count(kLargeId));
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, UnorderedMapKeyUsage) {
  std::unordered_map<TestDisplayId, int> unordered_map;
  unordered_map[kOne] = 1;
  unordered_map[kTwo] = 2;

  EXPECT_EQ(1, unordered_map[kOne]);
  EXPECT_EQ(2, unordered_map[kTwo]);
  EXPECT_EQ(0u, unordered_map.count(kLargeId));
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, CastToUnderlyingType) {
  EXPECT_EQ(1u, static_cast<uint64_t>(kOne));
  EXPECT_EQ(2u, static_cast<uint64_t>(kTwo));
  EXPECT_EQ(kLargeIdValue, static_cast<uint64_t>(kLargeId));
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, Value) {
  EXPECT_EQ(1u, kOne.value());
  EXPECT_EQ(2u, kTwo.value());
  EXPECT_EQ(kLargeIdValue, kLargeId.value());
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, ToFidl) {
  EXPECT_EQ(1u, kOne.ToFidl().value);
  EXPECT_EQ(2u, kTwo.ToFidl().value);
  EXPECT_EQ(kLargeIdValue, kLargeId.ToFidl().value);
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, ToBanjo) {
  EXPECT_EQ(1u, kOne.ToBanjo());
  EXPECT_EQ(2u, kTwo.ToBanjo());
  EXPECT_EQ(kLargeIdValue, kLargeId.ToBanjo());
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, FromBanjo) {
  EXPECT_EQ(kOne, TestDisplayId(uint64_t{1}));
  EXPECT_EQ(kTwo, TestDisplayId(uint64_t{2}));
  EXPECT_EQ(kLargeId, TestDisplayId(uint64_t{kLargeIdValue}));
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, FromFidl) {
  EXPECT_EQ(kOne, TestDisplayId(fuchsia_hardware_display_types::wire::DisplayId{.value = 1}));
  EXPECT_EQ(kTwo, TestDisplayId(fuchsia_hardware_display_types::wire::DisplayId{.value = 2}));
  EXPECT_EQ(kLargeId,
            TestDisplayId(fuchsia_hardware_display_types::wire::DisplayId{.value = kLargeIdValue}));
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, FidlConversionRoundtrip) {
  EXPECT_EQ(kOne, TestDisplayId(kOne.ToFidl()));
  EXPECT_EQ(kTwo, TestDisplayId(kTwo.ToFidl()));
  EXPECT_EQ(kLargeId, TestDisplayId(kLargeId.ToFidl()));
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kOne, TestDisplayId(kOne.ToBanjo()));
  EXPECT_EQ(kTwo, TestDisplayId(kTwo.ToBanjo()));
  EXPECT_EQ(kLargeId, TestDisplayId(kLargeId.ToBanjo()));
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, PreIncrement) {
  TestDisplayId display_id(1);
  TestDisplayId& preincrement_result = ++display_id;

  EXPECT_EQ(2u, display_id.value());
  EXPECT_EQ(&display_id, &preincrement_result);
}

TEST_F(IdTypeWithBanjoDefaultTraitsTest, PostIncrement) {
  TestDisplayId display_id(1);
  TestDisplayId postincrement_result = display_id++;

  EXPECT_EQ(2u, display_id.value());
  EXPECT_EQ(1u, postincrement_result.value());
}

class IdTypeWithoutBanjoDefaultTraitsTest : public ::testing::Test {
 protected:
  using TestConfigStampTraits =
      DefaultIdTypeTraits<uint64_t, fuchsia_hardware_display::wire::ConfigStamp, std::false_type>;
  using TestConfigStamp = IdType<TestConfigStampTraits>;

  static constexpr TestConfigStamp kOne = TestConfigStamp(1);
  static constexpr TestConfigStamp kAnotherOne = TestConfigStamp(1);
  static constexpr TestConfigStamp kTwo = TestConfigStamp(2);

  static constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
  static constexpr TestConfigStamp kLargeId = TestConfigStamp(kLargeIdValue);
};

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, OrderingForEqualValues) {
  EXPECT_FALSE(kOne < kAnotherOne);
  EXPECT_FALSE(kAnotherOne < kOne);

  EXPECT_LE(kOne, kAnotherOne);
  EXPECT_LE(kAnotherOne, kOne);

  EXPECT_FALSE(kOne > kAnotherOne);
  EXPECT_FALSE(kAnotherOne > kOne);

  EXPECT_GE(kOne, kAnotherOne);
  EXPECT_GE(kAnotherOne, kAnotherOne);
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, OrderingForDifferentValues) {
  EXPECT_LT(kOne, kTwo);
  EXPECT_FALSE(kTwo < kOne);

  EXPECT_LE(kOne, kTwo);
  EXPECT_FALSE(kTwo <= kOne);

  EXPECT_FALSE(kOne > kTwo);
  EXPECT_GT(kTwo, kOne);

  EXPECT_FALSE(kOne >= kTwo);
  EXPECT_GE(kTwo, kOne);
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, HashSpecialization) {
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{1}), std::hash<TestConfigStamp>()(kOne));
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{2}), std::hash<TestConfigStamp>()(kTwo));
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{kLargeIdValue}), std::hash<TestConfigStamp>()(kLargeId));
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, OrderedMapKeyUsage) {
  std::map<TestConfigStamp, int> ordered_map;
  ordered_map[kOne] = 1;
  ordered_map[kTwo] = 2;

  EXPECT_EQ(1, ordered_map[kOne]);
  EXPECT_EQ(2, ordered_map[kTwo]);
  EXPECT_EQ(0u, ordered_map.count(kLargeId));
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, UnorderedMapKeyUsage) {
  std::unordered_map<TestConfigStamp, int> unordered_map;
  unordered_map[kOne] = 1;
  unordered_map[kTwo] = 2;

  EXPECT_EQ(1, unordered_map[kOne]);
  EXPECT_EQ(2, unordered_map[kTwo]);
  EXPECT_EQ(0u, unordered_map.count(kLargeId));
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, ToFidl) {
  EXPECT_EQ(1u, kOne.ToFidl().value);
  EXPECT_EQ(2u, kTwo.ToFidl().value);
  EXPECT_EQ(kLargeIdValue, kLargeId.ToFidl().value);
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, FromBanjo) {
  EXPECT_EQ(kOne, TestConfigStamp(uint64_t{1}));
  EXPECT_EQ(kTwo, TestConfigStamp(uint64_t{2}));
  EXPECT_EQ(kLargeId, TestConfigStamp(uint64_t{kLargeIdValue}));
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, FromFidl) {
  EXPECT_EQ(kOne, TestConfigStamp(fuchsia_hardware_display::wire::ConfigStamp{.value = 1}));
  EXPECT_EQ(kTwo, TestConfigStamp(fuchsia_hardware_display::wire::ConfigStamp{.value = 2}));
  EXPECT_EQ(kLargeId,
            TestConfigStamp(fuchsia_hardware_display::wire::ConfigStamp{.value = kLargeIdValue}));
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, FidlConversionRoundtrip) {
  EXPECT_EQ(kOne, TestConfigStamp(kOne.ToFidl()));
  EXPECT_EQ(kTwo, TestConfigStamp(kTwo.ToFidl()));
  EXPECT_EQ(kLargeId, TestConfigStamp(kLargeId.ToFidl()));
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, PreIncrement) {
  TestConfigStamp config_stamp(1);
  TestConfigStamp& preincrement_result = ++config_stamp;

  EXPECT_EQ(2u, config_stamp.value());
  EXPECT_EQ(&config_stamp, &preincrement_result);
}

TEST_F(IdTypeWithoutBanjoDefaultTraitsTest, PostIncrement) {
  TestConfigStamp config_stamp(1);
  TestConfigStamp postincrement_result = config_stamp++;

  EXPECT_EQ(2u, config_stamp.value());
  EXPECT_EQ(1u, postincrement_result.value());
}

class IdTypeWithBanjoCustomTraitsTest : public ::testing::Test {
 protected:
  struct TestDriverConfigStampTraits
      : public DefaultIdTypeTraits<uint64_t, fuchsia_hardware_display_engine::wire::ConfigStamp,
                                   config_stamp_t> {
    static constexpr uint64_t FromBanjo(const config_stamp_t& banjo_config_stamp) noexcept {
      return banjo_config_stamp.value;
    }
    static constexpr config_stamp_t ToBanjo(const uint64_t& config_stamp_value) noexcept {
      return config_stamp_t{.value = config_stamp_value};
    }
  };
  using TestDriverConfigStamp = IdType<TestDriverConfigStampTraits>;

  static constexpr TestDriverConfigStamp kOne = TestDriverConfigStamp(1);
  static constexpr TestDriverConfigStamp kAnotherOne = TestDriverConfigStamp(1);
  static constexpr TestDriverConfigStamp kTwo = TestDriverConfigStamp(2);

  static constexpr uint64_t kLargeIdValue = uint64_t{1} << 63;
  static constexpr TestDriverConfigStamp kLargeId = TestDriverConfigStamp(kLargeIdValue);
};

TEST_F(IdTypeWithBanjoCustomTraitsTest, EqualityIsReflexive) {
  EXPECT_EQ(kOne, kOne);
  EXPECT_EQ(kAnotherOne, kAnotherOne);
  EXPECT_EQ(kTwo, kTwo);
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, EqualityIsSymmetric) {
  EXPECT_EQ(kOne, kAnotherOne);
  EXPECT_EQ(kAnotherOne, kOne);
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, EqualityForDifferentValues) {
  EXPECT_NE(kOne, kTwo);
  EXPECT_NE(kAnotherOne, kTwo);
  EXPECT_NE(kTwo, kOne);
  EXPECT_NE(kTwo, kAnotherOne);
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, OrderingForEqualValues) {
  EXPECT_FALSE(kOne < kAnotherOne);
  EXPECT_FALSE(kAnotherOne < kOne);

  EXPECT_LE(kOne, kAnotherOne);
  EXPECT_LE(kAnotherOne, kOne);

  EXPECT_FALSE(kOne > kAnotherOne);
  EXPECT_FALSE(kAnotherOne > kOne);

  EXPECT_GE(kOne, kAnotherOne);
  EXPECT_GE(kAnotherOne, kAnotherOne);
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, OrderingForDifferentValues) {
  EXPECT_LT(kOne, kTwo);
  EXPECT_FALSE(kTwo < kOne);

  EXPECT_LE(kOne, kTwo);
  EXPECT_FALSE(kTwo <= kOne);

  EXPECT_FALSE(kOne > kTwo);
  EXPECT_GT(kTwo, kOne);

  EXPECT_FALSE(kOne >= kTwo);
  EXPECT_GE(kTwo, kOne);
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, HashSpecialization) {
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{1}), std::hash<TestDriverConfigStamp>()(kOne));
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{2}), std::hash<TestDriverConfigStamp>()(kTwo));
  EXPECT_EQ(std::hash<uint64_t>()(uint64_t{kLargeIdValue}),
            std::hash<TestDriverConfigStamp>()(kLargeId));
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, OrderedMapKeyUsage) {
  std::map<TestDriverConfigStamp, int> ordered_map;
  ordered_map[kOne] = 1;
  ordered_map[kTwo] = 2;

  EXPECT_EQ(1, ordered_map[kOne]);
  EXPECT_EQ(2, ordered_map[kTwo]);
  EXPECT_EQ(0u, ordered_map.count(kLargeId));
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, UnorderedMapKeyUsage) {
  std::unordered_map<TestDriverConfigStamp, int> unordered_map;
  unordered_map[kOne] = 1;
  unordered_map[kTwo] = 2;

  EXPECT_EQ(1, unordered_map[kOne]);
  EXPECT_EQ(2, unordered_map[kTwo]);
  EXPECT_EQ(0u, unordered_map.count(kLargeId));
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, CastToUnderlyingType) {
  EXPECT_EQ(1u, static_cast<uint64_t>(kOne));
  EXPECT_EQ(2u, static_cast<uint64_t>(kTwo));
  EXPECT_EQ(kLargeIdValue, static_cast<uint64_t>(kLargeId));
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, Value) {
  EXPECT_EQ(1u, kOne.value());
  EXPECT_EQ(2u, kTwo.value());
  EXPECT_EQ(kLargeIdValue, kLargeId.value());
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, ToFidl) {
  EXPECT_EQ(1u, kOne.ToFidl().value);
  EXPECT_EQ(2u, kTwo.ToFidl().value);
  EXPECT_EQ(kLargeIdValue, kLargeId.ToFidl().value);
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, ToBanjo) {
  EXPECT_EQ(1u, kOne.ToBanjo().value);
  EXPECT_EQ(2u, kTwo.ToBanjo().value);
  EXPECT_EQ(kLargeIdValue, kLargeId.ToBanjo().value);
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, FromBanjo) {
  EXPECT_EQ(kOne, TestDriverConfigStamp(config_stamp_t{.value = 1}));
  EXPECT_EQ(kTwo, TestDriverConfigStamp(config_stamp_t{.value = 2}));
  EXPECT_EQ(kLargeId, TestDriverConfigStamp(config_stamp_t{.value = kLargeIdValue}));
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, FromFidl) {
  EXPECT_EQ(kOne,
            TestDriverConfigStamp(fuchsia_hardware_display_engine::wire::ConfigStamp{.value = 1}));
  EXPECT_EQ(kTwo,
            TestDriverConfigStamp(fuchsia_hardware_display_engine::wire::ConfigStamp{.value = 2}));
  EXPECT_EQ(kLargeId, TestDriverConfigStamp(fuchsia_hardware_display_engine::wire::ConfigStamp{
                          .value = kLargeIdValue}));
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, FidlConversionRoundtrip) {
  EXPECT_EQ(kOne, TestDriverConfigStamp(kOne.ToFidl()));
  EXPECT_EQ(kTwo, TestDriverConfigStamp(kTwo.ToFidl()));
  EXPECT_EQ(kLargeId, TestDriverConfigStamp(kLargeId.ToFidl()));
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, BanjoConversionRoundtrip) {
  EXPECT_EQ(kOne, TestDriverConfigStamp(kOne.ToBanjo()));
  EXPECT_EQ(kTwo, TestDriverConfigStamp(kTwo.ToBanjo()));
  EXPECT_EQ(kLargeId, TestDriverConfigStamp(kLargeId.ToBanjo()));
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, PreIncrement) {
  TestDriverConfigStamp driver_config_stamp(1);
  TestDriverConfigStamp& preincrement_result = ++driver_config_stamp;

  EXPECT_EQ(2u, driver_config_stamp.value());
  EXPECT_EQ(&driver_config_stamp, &preincrement_result);
}

TEST_F(IdTypeWithBanjoCustomTraitsTest, PostIncrement) {
  TestDriverConfigStamp driver_config_stamp(1);
  TestDriverConfigStamp postincrement_result = driver_config_stamp++;

  EXPECT_EQ(2u, driver_config_stamp.value());
  EXPECT_EQ(1u, postincrement_result.value());
}

}  // namespace

}  // namespace display::internal
