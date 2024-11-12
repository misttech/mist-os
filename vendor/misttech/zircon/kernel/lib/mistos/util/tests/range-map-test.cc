// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/range-map.h>
#include <stdint.h>

#include <optional>

#include <zxtest/zxtest.h>

namespace util {
namespace {

using TestRange = util::Range<uint64_t>;
using TestRangeMap = RangeMap<uint64_t, int32_t>;

TEST(RangeMapTest, EmptyMap) {
  TestRangeMap map;

  EXPECT_FALSE(map.get(12).has_value());
  map.remove({10, 34});
  // This is a test to make sure we can handle reversed ranges
  map.remove({34, 10});
}

TEST(RangeMapTest, InsertIntoEmpty) {
  TestRangeMap map;

  map.insert({10, 34}, -14);

  ASSERT_EQ((std::make_pair(TestRange({10, 34}), -14)), map.get(12));
  ASSERT_EQ((std::make_pair(TestRange({10, 34}), -14)), map.get(10));
  ASSERT_FALSE(map.get(9).has_value());
  ASSERT_EQ((std::make_pair(TestRange({10, 34}), -14)), map.get(33));
  ASSERT_FALSE(map.get(34).has_value());
}

TEST(RangeMapTest, InsertStruct) {
  struct Mapping {
    // Mapping() = default;
    // Mapping(Mapping&&) = default;
    // Mapping& operator=(Mapping&&) { return *this; }

    // DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(Mapping);
    bool operator==(Mapping const& other) const { return true; }
  };

  RangeMap<uint64_t, Mapping> map;
  Mapping m;

  map.insert({10, 34}, m);
}

TEST(RangeMapTest, Iteration) {
  TestRangeMap map;

  map.insert({10, 34}, -14);
  map.insert({74, 92}, -12);

  {
    auto new_map = map.iter();
    auto it = new_map.begin();

    ASSERT_EQ((std::make_pair(TestRange({10, 34}), -14)), *it);
    ASSERT_EQ((std::make_pair(TestRange({74, 92}), -12)), *++it);
    ASSERT_EQ(++it, new_map.end());
  }

  {
    auto new_map = map.iter_starting_at(10);
    auto it = new_map.begin();

    ASSERT_EQ((std::make_pair(TestRange({10, 34}), -14)), *it);
  }

  {
    auto new_map = map.iter_starting_at(11);
    auto it = new_map.begin();

    ASSERT_EQ((std::make_pair(TestRange({74, 92}), -12)), *it);
  }

  {
    auto new_map = map.iter_starting_at(74);
    auto it = new_map.begin();

    ASSERT_EQ((std::make_pair(TestRange({74, 92}), -12)), *it);
  }

  {
    auto new_map = map.iter_starting_at(75);
    auto it = new_map.begin();
    ASSERT_EQ(it, new_map.end());
  }
}

TEST(RangeMapTest, RemoveOverlappingEdge) {
  TestRangeMap map;

  map.insert({10, 34}, -14);

  map.remove({2, 11});
  ASSERT_EQ((std::make_pair(TestRange({11, 34}), -14)), map.get(11));

  map.remove({33, 42});
  ASSERT_EQ((std::make_pair(TestRange({11, 33}), -14)), map.get(12));
}

TEST(RangeMapTest, RemoveMiddleSplitsRange) {
  TestRangeMap map;
  TestRange range1{10, 34};

  map.insert(range1, -14);
  map.remove({15, 18});

  ASSERT_EQ((std::make_pair(TestRange({10, 15}), -14)), map.get(12));
  ASSERT_EQ((std::make_pair(TestRange({18, 34}), -14)), map.get(20));
}

TEST(RangeMapTest, RemoveUpperHalfOfSplitRangeLeavesLowerRange) {
  TestRangeMap map;

  map.insert({10, 34}, -14);
  map.remove({15, 18});
  map.insert({2, 7}, -21);
  map.remove({20, 42});

  ASSERT_EQ((std::make_pair(TestRange({2, 7}), -21)), map.get(5));
  ASSERT_EQ((std::make_pair(TestRange({10, 15}), -14)), map.get(12));
}

TEST(RangeMapTest, RangeMapOverlappingInsert) {
  TestRangeMap map;

  map.insert({2, 7}, -21);
  map.insert({5, 9}, -42);
  map.insert({1, 3}, -43);
  map.insert({6, 8}, -44);

  ASSERT_EQ((std::make_pair(TestRange({1, 3}), -43)), map.get(2));
  ASSERT_EQ((std::make_pair(TestRange({3, 5}), -21)), map.get(4));
  ASSERT_EQ((std::make_pair(TestRange({5, 6}), -42)), map.get(5));
  ASSERT_EQ((std::make_pair(TestRange({6, 8}), -44)), map.get(7));
}

TEST(RangeMapTest, IntersectSingle) {
  TestRangeMap map;

  map.insert({2, 7}, -10);

  {
    TestRangeMap::RangeType range{3, 4};
    auto new_map = map.intersection(range);

    auto it = new_map.begin();
    ASSERT_EQ((std::make_pair(TestRange({2, 7}), -10)), *it);
    ASSERT_EQ(++it, new_map.end());
  }

  {
    TestRangeMap::RangeType range{2, 3};
    auto new_map = map.intersection(range);

    auto it = new_map.begin();
    ASSERT_EQ((std::make_pair(TestRange({2, 7}), -10)), *it);
    ASSERT_EQ(++it, new_map.end());
  }

  {
    TestRangeMap::RangeType range{1, 4};
    auto new_map = map.intersection(range);

    auto it = new_map.begin();
    ASSERT_EQ((std::make_pair(TestRange({2, 7}), -10)), *it);
    ASSERT_EQ(++it, new_map.end());
  }

  {
    TestRangeMap::RangeType range{1, 2};
    auto new_map = map.intersection(range);

    auto it = new_map.begin();
    ASSERT_EQ(++it, new_map.end());
  }

  {
    TestRangeMap::RangeType range{6, 7};
    auto new_map = map.intersection(range);

    auto it = new_map.begin();
    ASSERT_EQ(++it, new_map.end());
  }
}

TEST(RangeMapTest, IntersectMultiple) {
  TestRangeMap map;

  map.insert({2, 7}, -10);
  map.insert({7, 9}, -20);
  map.insert({10, 11}, -30);

  {
    TestRangeMap::RangeType range{3, 8};
    auto new_map = map.intersection(range);
    auto it = new_map.begin();

    ASSERT_EQ(new_map.size(), 2);
    ASSERT_EQ((std::make_pair(TestRange({2, 7}), -10)), *it);
    ASSERT_EQ((std::make_pair(TestRange({7, 9}), -20)), *++it);
    ASSERT_EQ(++it, new_map.end());
  }

  {
    TestRangeMap::RangeType range{3, 11};
    auto new_map = map.intersection(range);
    auto it = new_map.begin();

    ASSERT_EQ((std::make_pair(TestRange({2, 7}), -10)), *it);
    ASSERT_EQ((std::make_pair(TestRange({7, 9}), -20)), *++it);
    ASSERT_EQ((std::make_pair(TestRange({10, 11}), -30)), *++it);
    ASSERT_EQ(++it, new_map.end());
  }
}

TEST(RangeMapTest, IntersectNoGaps) {
  TestRangeMap map;

  map.insert({0, 1}, -10);
  map.insert({1, 2}, -20);
  map.insert({2, 3}, -30);

  TestRangeMap::RangeType range{0, 3};
  auto new_map = map.intersection(range);
  auto it = new_map.begin();

  ASSERT_EQ((std::make_pair(TestRange({0, 1}), -10)), *it);
  ASSERT_EQ((std::make_pair(TestRange({1, 2}), -20)), *++it);
  ASSERT_EQ((std::make_pair(TestRange({2, 3}), -30)), *++it);
  ASSERT_EQ(++it, new_map.end());
}

TEST(RangeMapTest, Merging) {
  TestRangeMap map;

  map.insert({1, 2}, -10);

  {
    auto new_map = map.iter();

    std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>> result;
    std::transform(new_map.begin(), new_map.end(), std::inserter(result, result.end()),
                   [](const auto& pair) { return pair; });

    ASSERT_EQ((std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>>{
                  {TestRange({1, 2}), -10}}),
              result);
  }

  map.insert({3, 4}, -10);
  {
    auto new_map = map.iter();

    std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>> result;
    std::transform(new_map.begin(), new_map.end(), std::inserter(result, result.end()),
                   [](const auto& pair) { return pair; });

    ASSERT_EQ((std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>>{
                  {TestRange({1, 2}), -10}, {TestRange({3, 4}), -10}}),
              result);
  }

  map.insert({2, 3}, -10);
  {
    auto new_map = map.iter();

    std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>> result;
    std::transform(new_map.begin(), new_map.end(), std::inserter(result, result.end()),
                   [](const auto& pair) { return pair; });

    ASSERT_EQ((std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>>{
                  {TestRange({1, 4}), -10}}),
              result);
  }

  map.insert({0, 1}, -10);
  {
    auto new_map = map.iter();

    std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>> result;
    std::transform(new_map.begin(), new_map.end(), std::inserter(result, result.end()),
                   [](const auto& pair) { return pair; });

    ASSERT_EQ((std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>>{
                  {TestRange({0, 4}), -10}}),
              result);
  }

  map.insert({4, 5}, -10);
  {
    auto new_map = map.iter();

    std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>> result;
    std::transform(new_map.begin(), new_map.end(), std::inserter(result, result.end()),
                   [](const auto& pair) { return pair; });

    ASSERT_EQ((std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>>{
                  {TestRange({0, 5}), -10}}),
              result);
  }

  map.insert({2, 3}, -20);
  {
    auto new_map = map.iter();

    std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>> result;
    std::transform(new_map.begin(), new_map.end(), std::inserter(result, result.end()),
                   [](const auto& pair) { return pair; });

    ASSERT_EQ((std::vector<std::pair<TestRangeMap::RangeType, TestRangeMap::ValueType>>{
                  {TestRange({0, 2}), -10}, {TestRange({2, 3}), -20}, {TestRange({3, 5}), -10}}),
              result);
  }
}

}  // namespace
}  // namespace util
