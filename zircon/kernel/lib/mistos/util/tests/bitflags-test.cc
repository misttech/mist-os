// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/bitflags.h>

#include <zxtest/zxtest.h>

enum class TestEnum : uint8_t {
  /// 1
  A = 1,

  /// 1 << 1
  B = 1 << 1,

  /// 1 << 2
  C = 1 << 2,

  /// 1 | (1 << 1) | (1 << 2)
  ABC = A | B | C,
};

template <>
constexpr Flag<TestEnum> Flags<TestEnum>::FLAGS[] = {
    {TestEnum::A},
    {TestEnum::B},
    {TestEnum::C},
};
using TestFlags = Flags<TestEnum>;

enum class TestZero : uint8_t {
  Zero = 0,
};

template <>
constexpr Flag<TestZero> Flags<TestZero>::FLAGS[] = {{TestZero::Zero}};
using TestZeroFlags = Flags<TestZero>;

enum class TestZeroOne : uint8_t {
  Zero = 0,
  One = 1,
};

template <>
constexpr Flag<TestZeroOne> Flags<TestZeroOne>::FLAGS[] = {{TestZeroOne::Zero}, {TestZeroOne::One}};
using TestZeroOneFlags = Flags<TestZeroOne>;

enum class TestEmpty : uint8_t {};

template <>
constexpr Flag<TestEmpty> Flags<TestEmpty>::FLAGS[] = {};
using TestEmptyFlags = Flags<TestEmpty>;

enum class TestOverlapping : uint8_t {
  /// 1 | (1 << 1)
  AB = 1 | (1 << 1),

  /// (1 << 1) | (1 << 2)
  BC = (1 << 1) | (1 << 2),
};

template <>
constexpr Flag<TestOverlapping> Flags<TestOverlapping>::FLAGS[] = {{TestOverlapping::AB},
                                                                   {TestOverlapping::BC}};
using TestOverlappingFlags = Flags<TestOverlapping>;

enum class TestOverlappingFull : uint8_t {
  A = 1,
  B = 1,
  C = 1,
  D = 1 << 1,
};

template <>
constexpr Flag<TestOverlappingFull> Flags<TestOverlappingFull>::FLAGS[] = {
    {TestOverlappingFull::A},
    {TestOverlappingFull::B},
    {TestOverlappingFull::C},
    {TestOverlappingFull::D}};
using TestOverlappingFullFlags = Flags<TestOverlappingFull>;

namespace {

TEST(BitFlags, All) {
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::all().bits());
  ASSERT_EQ(0, TestZeroFlags::all().bits());
  ASSERT_EQ(0, TestEmptyFlags::all().bits());
}

TEST(BitFlags, Bits) {
  ASSERT_EQ(0, TestFlags::empty().bits());
  ASSERT_EQ(static_cast<uint8_t>(~0), TestFlags::from_bits_retain(UINT8_MAX).bits());
  ASSERT_EQ(1 << 3, TestFlags::from_bits_retain(1 << 3).bits());
  ASSERT_EQ(1 << 3, TestZeroFlags::from_bits_retain(1 << 3).bits());
  ASSERT_EQ(1 << 3, TestEmptyFlags::from_bits_retain(1 << 3).bits());
}

TEST(BitFlags, Complement) {
  ASSERT_EQ(0, TestFlags::all().complement().bits());
  ASSERT_EQ(0, TestFlags::from_bits_retain(~0).complement().bits());
  ASSERT_EQ(1 | 1 << 1, TestFlags(TestEnum::C).complement().bits());
  ASSERT_EQ(1 | 1 << 1,
            (TestFlags(TestEnum::C) | TestFlags::from_bits_retain(1 << 3)).complement().bits());
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::empty().complement().bits());
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::from_bits_retain(1 << 3).complement().bits());

  ASSERT_EQ(0, TestZeroFlags::empty().complement().bits());
  ASSERT_EQ(0, TestEmptyFlags::empty().complement().bits());
  ASSERT_EQ(1 << 2, TestOverlappingFlags(TestOverlapping::AB).complement().bits());
}

TEST(BitFlags, Complement_operator) {
  ASSERT_EQ(0, (!TestFlags::all()).bits());
  /*ASSERT_EQ(0, TestFlags::from_bits_retain(~0).complement().bits());
  ASSERT_EQ(1 | 1 << 1, TestFlags(TestEnum::C).complement().bits());
  ASSERT_EQ(1 | 1 << 1,
            (TestFlags(TestEnum::C) | TestFlags::from_bits_retain(1 << 3)).complement().bits());
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::empty().complement().bits());
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::from_bits_retain(1 << 3).complement().bits());

  ASSERT_EQ(0, TestZeroFlags::empty().complement().bits());
  ASSERT_EQ(0, TestEmptyFlags::empty().complement().bits());
  ASSERT_EQ(1 << 2, TestOverlappingFlags(TestOverlapping::AB).complement().bits());*/
}

TEST(BitFlags, Contains) {
  ASSERT_TRUE(TestFlags::empty().contains(TestFlags::empty()));
  ASSERT_FALSE(TestFlags::empty().contains(TestEnum::A));
  ASSERT_FALSE(TestFlags::empty().contains(TestEnum::B));
  ASSERT_FALSE(TestFlags::empty().contains(TestEnum::C));
  ASSERT_FALSE(TestFlags::empty().contains(TestFlags::from_bits_retain(1 << 3)));

  TestFlags flagA(TestEnum::A);
  ASSERT_TRUE(flagA.contains(TestFlags::empty()));
  ASSERT_TRUE(flagA.contains(TestEnum::A));
  ASSERT_FALSE(flagA.contains(TestEnum::B));
  ASSERT_FALSE(flagA.contains(TestEnum::C));
  ASSERT_FALSE(flagA.contains(TestEnum::ABC));
  ASSERT_FALSE(flagA.contains(TestFlags::from_bits_retain(1 << 3)));
  ASSERT_FALSE(flagA.contains(TestFlags::from_bits_retain(1 | 1 << 3)));

  TestZeroFlags flagZero(TestZero::Zero);
  ASSERT_TRUE(flagZero.contains(TestZero::Zero));

  auto test = TestFlags::from_bits_retain(1 << 3);
  ASSERT_TRUE(test.contains(TestFlags::empty()));
  ASSERT_FALSE(test.contains(TestEnum::A));
  ASSERT_FALSE(test.contains(TestEnum::B));
  ASSERT_FALSE(test.contains(TestEnum::C));
  ASSERT_TRUE(test.contains(TestFlags::from_bits_retain(1 << 3)));

  TestOverlappingFlags value(TestOverlapping::AB);
  ASSERT_TRUE(value.contains(TestOverlapping::AB));
  ASSERT_FALSE(value.contains(TestOverlapping::BC));
  ASSERT_TRUE(value.contains(TestOverlappingFlags::from_bits_retain(1 << 1)));
}

TEST(BitFlags, Difference) {
  ASSERT_EQ(
      1 << 1,
      (TestFlags(TestEnum::A) | TestFlags(TestEnum::B)).difference(TestFlags(TestEnum::A)).bits());
  ASSERT_EQ(
      1,
      (TestFlags(TestEnum::A) | TestFlags(TestEnum::B)).difference(TestFlags(TestEnum::B)).bits());
  ASSERT_EQ(1 | 1 << 1, (TestFlags(TestEnum::A) | TestFlags(TestEnum::B))
                            .difference(TestFlags::from_bits_retain(1 << 3))
                            .bits());

  ASSERT_EQ(1 << 3,
            TestFlags::from_bits_retain(1 | 1 << 3).difference(TestFlags(TestEnum::A)).bits());
  ASSERT_EQ(1, TestFlags::from_bits_retain(1 | 1 << 3)
                   .difference(TestFlags::from_bits_retain(1 << 3))
                   .bits());

  ASSERT_EQ(0b11111110, TestFlags::from_bits_retain(~0).difference(TestEnum::A).bits());
  ASSERT_EQ(1 << 1 | 1 << 2, (TestFlags::from_bits_retain(~0) & !TestFlags(TestEnum::A)).bits());
}

TEST(BitFlags, Difference_operator) {
  ASSERT_EQ(1 << 1,
            ((TestFlags(TestEnum::A) | TestFlags(TestEnum::B)) - TestFlags(TestEnum::A)).bits());
  ASSERT_EQ(1, ((TestFlags(TestEnum::A) | TestFlags(TestEnum::B)) - TestFlags(TestEnum::B)).bits());
  ASSERT_EQ(1 | 1 << 1, ((TestFlags(TestEnum::A) | TestFlags(TestEnum::B)) -
                         TestFlags::from_bits_retain(1 << 3))
                            .bits());
  ASSERT_EQ(1 << 1, []() -> auto {
    auto value = TestFlags(TestEnum::A) | TestFlags(TestEnum::B);
    value -= TestEnum::A;
    return value.bits();
  }());

  ASSERT_EQ(1, []() -> auto {
    auto value = TestFlags(TestEnum::A) | TestFlags(TestEnum::B);
    value -= TestEnum::B;
    return value.bits();
  }());

  ASSERT_EQ(1 | 1 << 1, []() -> auto {
    auto value = TestFlags(TestEnum::A) | TestFlags(TestEnum::B);
    value -= TestFlags::from_bits_retain(1 << 3);
    return value.bits();
  }());
}

TEST(BitFlags, Eq) {
  ASSERT_EQ(TestFlags::empty(), TestFlags::empty());
  ASSERT_EQ(TestFlags::all(), TestFlags::all());

  /*
    assert!(TestFlags::from_bits_retain(1) < TestFlags::from_bits_retain(2));
    assert!(TestFlags::from_bits_retain(2) > TestFlags::from_bits_retain(1));
  */
}

TEST(BitFlags, from_bits_retain) {
  ASSERT_EQ(0, TestFlags::from_bits_retain(0).bits());
  ASSERT_EQ(1, TestFlags::from_bits_retain(1).bits());
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::from_bits_retain(1 | 1 << 1 | 1 << 2).bits());
  ASSERT_EQ(1 << 3, TestFlags::from_bits_retain(1 << 3).bits());
  ASSERT_EQ(1 | 1 << 3, TestFlags::from_bits_retain(1 | 1 << 3).bits());

  ASSERT_EQ(1 << 1, TestOverlappingFlags::from_bits_retain(1 << 1).bits());
  ASSERT_EQ(1 << 5, TestOverlappingFlags::from_bits_retain(1 << 5).bits());
}

TEST(BitFlags, from_bits_truncate) {
  ASSERT_EQ(0, TestFlags::from_bits_truncate(0).bits());
  ASSERT_EQ(1, TestFlags::from_bits_truncate(1).bits());
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::from_bits_truncate(1 | 1 << 1 | 1 << 2).bits());
  ASSERT_EQ(0, TestFlags::from_bits_truncate(1 << 3).bits());
  ASSERT_EQ(1, TestFlags::from_bits_truncate(1 | 1 << 3).bits());

  ASSERT_EQ(1 | 1 << 1, TestOverlappingFlags::from_bits_truncate(1 | 1 << 1).bits());
  ASSERT_EQ(1 << 1, TestOverlappingFlags::from_bits_truncate(1 << 1).bits());
}

TEST(BitFlags, Intersection) {
  ASSERT_EQ(0, TestFlags::empty().intersection(TestFlags::empty()).bits());
  ASSERT_EQ(0, TestFlags::empty().intersection(TestFlags::all()).bits());

  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::all().intersection(TestFlags::all()).bits());
  ASSERT_EQ(1, TestFlags::all().intersection(TestFlags(TestEnum::A)).bits());
  ASSERT_EQ(0, TestFlags::all().intersection(TestFlags::from_bits_retain(1 << 3)).bits());

  ASSERT_EQ(
      1 << 3,
      TestFlags::from_bits_retain(1 << 3).intersection(TestFlags::from_bits_retain(1 << 3)).bits());

  ASSERT_EQ(1 << 1, TestOverlappingFlags(TestOverlapping::AB)
                        .intersection(TestOverlappingFlags(TestOverlapping::BC))
                        .bits());
}

TEST(BitFlags, Intersection_operator) {
  ASSERT_EQ(0, (TestFlags::empty() & TestFlags::empty()).bits());
  ASSERT_EQ(0, (TestFlags::empty() & TestFlags::all()).bits());

  ASSERT_EQ(1 | 1 << 1 | 1 << 2, (TestFlags::all() & TestFlags::all()).bits());
  ASSERT_EQ(1, (TestFlags::all() & TestFlags(TestEnum::A)).bits());
  ASSERT_EQ(0, (TestFlags::all() & TestFlags::from_bits_retain(1 << 3)).bits());

  ASSERT_EQ(1 << 3,
            (TestFlags::from_bits_retain(1 << 3) & (TestFlags::from_bits_retain(1 << 3))).bits());

  ASSERT_EQ(1 << 1,
            (TestOverlappingFlags(TestOverlapping::AB) & TestOverlappingFlags(TestOverlapping::BC))
                .bits());

  ASSERT_EQ(0, [&]() -> auto {
    auto value = TestFlags::empty();
    value &= TestFlags::empty();
    return value.bits();
  }());
  ASSERT_EQ(0, [&]() -> auto {
    auto value = TestFlags::empty();
    value &= TestFlags::all();
    return value.bits();
  }());

  ASSERT_EQ(1 | 1 << 1 | 1 << 2, [&]() -> auto {
    auto value = TestFlags::all();
    value &= TestFlags::all();
    return value.bits();
  }());
  ASSERT_EQ(1, [&]() -> auto {
    auto value = TestFlags::all();
    value &= TestEnum::A;
    return value.bits();
  }());
  ASSERT_EQ(0, [&]() -> auto {
    auto value = TestFlags::all();
    value &= TestFlags::from_bits_retain(1 << 3);
    return value.bits();
  }());

  ASSERT_EQ(1 << 3, [&]() -> auto {
    auto value = TestFlags::from_bits_retain(1 << 3);
    value &= TestFlags::from_bits_retain(1 << 3);
    return value.bits();
  }());

  ASSERT_EQ(1 << 1, []() -> auto {
    auto value = TestOverlappingFlags(TestOverlapping::AB);
    value &= TestOverlapping::BC;
    return value.bits();
  }());
}

TEST(BitFlags, Union_empty) {
  ASSERT_EQ(1, TestFlags::empty().union_(TestEnum::A).bits());
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, TestFlags::empty().union_(TestFlags::all()).bits());
  ASSERT_EQ(0, TestFlags::empty().union_(TestFlags::empty()).bits());
  ASSERT_EQ(1 << 3, TestFlags::empty().union_(TestFlags::from_bits_retain(1 << 3)).bits());

  ASSERT_EQ(1, (TestFlags::empty() | TestFlags(TestEnum::A)).bits());
  ASSERT_EQ(1 | 1 << 1 | 1 << 2, (TestFlags::empty() | TestFlags::all()).bits());
  ASSERT_EQ(0, (TestFlags::empty() | TestFlags::empty()).bits());
  ASSERT_EQ(1 << 3, (TestFlags::empty() | TestFlags::from_bits_retain(1 << 3)).bits());

  ASSERT_EQ(1, []() -> auto {
    auto value = TestFlags::empty();
    value |= TestFlags(TestEnum::A);
    return value.bits();
  }());

  ASSERT_EQ(1 | 1 << 1 | 1 << 2, []() -> auto {
    auto value = TestFlags::empty();
    value |= TestFlags::all();
    return value.bits();
  }());

  ASSERT_EQ(0, []() -> auto {
    auto value = TestFlags::empty();
    value |= TestFlags::empty();
    return value.bits();
  }());

  ASSERT_EQ(1 << 3, []() -> auto {
    auto value = TestFlags::empty();
    value |= TestFlags::from_bits_retain(1 << 3);
    return value.bits();
  }());
}

#define union_value (TestFlags(TestEnum::A) | TestFlags(TestEnum::C))

TEST(BitFlags, Union_operator) {
  ASSERT_EQ(1 | 1 << 1 | 1 << 2,
            union_value.union_(TestFlags(TestEnum::A) | TestFlags(TestEnum::B)).bits());
  ASSERT_EQ(1 | 1 << 2, union_value.union_(TestFlags(TestEnum::A)).bits());

  ASSERT_EQ(1 | 1 << 1 | 1 << 2,
            ((union_value | (TestFlags(TestEnum::A) | TestFlags(TestEnum::B))).bits()));
  ASSERT_EQ(1 | 1 << 2, (union_value | TestFlags(TestEnum::A)).bits());

  ASSERT_EQ(1 | 1 << 1 | 1 << 2, []() -> auto {
    auto value = union_value;
    value |= (TestFlags(TestEnum::A) | TestFlags(TestEnum::B));
    return value.bits();
  }());

  ASSERT_EQ(1 | 1 << 2, []() -> auto {
    auto value = union_value;
    value |= TestFlags(TestEnum::A);
    return value.bits();
  }());
}

}  // namespace
