// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/dwarf/section-data.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/elfldltl/testing/typed-test.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

namespace {

FORMAT_TYPED_TEST_SUITE(ElfldltlDwarfTests);

template <class Elf>
struct InitialLength32 {
  typename Elf::Word length = 0;
};

template <class Elf>
struct [[gnu::packed]] InitialLength64 {
  constexpr InitialLength64() = default;

  constexpr explicit InitialLength64(uint64_t n) : length{n} {}

  [[gnu::packed]] const uint32_t dwarf64_marker = 0xffffffff;
  [[gnu::packed]] typename Elf::Xword length = 0;
};

template <class Length, typename T>
struct [[gnu::packed]] TestData {
  constexpr TestData() = default;

  explicit constexpr TestData(const T& data) : contents{data} {}

  [[gnu::packed]] Length initial_length{sizeof(T)};
  [[gnu::packed]] T contents{};
};

template <typename T>
constexpr cpp20::span<const std::byte> AsBytes(T& data) {
  return cpp20::as_bytes(cpp20::span{&data, 1});
}

constexpr elfldltl::FileAddress kErrorArg{0x123u};

constexpr cpp20::span<const std::byte> kNoBytes{};

TYPED_TEST(ElfldltlDwarfTests, SectionDataEmpty) {
  using Elf = typename TestFixture::Elf;

  elfldltl::dwarf::SectionData section_data;
  EXPECT_TRUE(section_data.contents().empty());

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  {
    constexpr InitialLength32<Elf> kEmpty;
    auto read = elfldltl::dwarf::SectionData::Read<Elf>(diag, AsBytes(kEmpty));
    ASSERT_TRUE(read);
    EXPECT_EQ(read->size_bytes(), 4u);
    EXPECT_EQ(read->contents().size_bytes(), 0u);
  }

  {
    constexpr InitialLength64<Elf> kEmpty;
    auto read = elfldltl::dwarf::SectionData::Read<Elf>(diag, AsBytes(kEmpty));
    ASSERT_TRUE(read);
    EXPECT_EQ(read->size_bytes(), 12u);
    EXPECT_EQ(read->contents().size_bytes(), 0u);
  }
}

TYPED_TEST(ElfldltlDwarfTests, SectionDataRead) {
  using Elf = typename TestFixture::Elf;

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  static constexpr std::byte kOneByte{17};
  {
    constexpr TestData<InitialLength32<Elf>, std::byte> kOneByteData{kOneByte};
    auto read = elfldltl::dwarf::SectionData::Read<Elf>(diag, AsBytes(kOneByteData));
    ASSERT_TRUE(read);
    EXPECT_EQ(read->size_bytes(), sizeof(kOneByteData));
    EXPECT_EQ(read->format(), elfldltl::dwarf::SectionData::Format::kDwarf32);
    EXPECT_EQ(read->initial_length_size(), 4u);
    EXPECT_EQ(read->offset_size(), 4u);
    EXPECT_EQ(read->contents().size(), 1u);
    EXPECT_THAT(read->contents(), ::testing::ElementsAre(kOneByte));
  }
  {
    constexpr TestData<InitialLength64<Elf>, std::byte> kOneByteData{kOneByte};
    auto read = elfldltl::dwarf::SectionData::Read<Elf>(diag, AsBytes(kOneByteData));
    ASSERT_TRUE(read);
    EXPECT_EQ(read->size_bytes(), sizeof(kOneByteData));
    EXPECT_EQ(read->format(), elfldltl::dwarf::SectionData::Format::kDwarf64);
    EXPECT_EQ(read->initial_length_size(), 12u);
    EXPECT_EQ(read->offset_size(), 8u);
    EXPECT_EQ(read->contents().size(), 1u);
    EXPECT_THAT(read->contents(), ::testing::ElementsAre(kOneByte));
  }
}

TYPED_TEST(ElfldltlDwarfTests, SectionDataConsume) {
  using Elf = typename TestFixture::Elf;

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  static constexpr std::byte kOneByte{17};
  constexpr struct {
    TestData<InitialLength32<Elf>, std::byte> data{kOneByte};
    std::array<std::byte, 3> rest{std::byte{1}, std::byte{2}, std::byte{3}};
  } kData;

  auto [read, rest] = elfldltl::dwarf::SectionData::Consume<Elf>(diag, AsBytes(kData));
  ASSERT_TRUE(read);
  EXPECT_EQ(read->contents().size(), 1u);
  EXPECT_THAT(read->contents(), ::testing::ElementsAre(kOneByte));
  EXPECT_THAT(rest, ::testing::ElementsAreArray(kData.rest));
}

TYPED_TEST(ElfldltlDwarfTests, SectionDataReadOffset) {
  using Elf = typename TestFixture::Elf;

  auto diag = elfldltl::testing::ExpectOkDiagnostics();

  {
    struct [[gnu::packed]] DataWithOffset {
      std::byte extra_byte{23};
      [[gnu::packed]] typename Elf::Word offset{1234};
    };
    constexpr TestData<InitialLength32<Elf>, DataWithOffset> kDataWithOffset;
    auto read = elfldltl::dwarf::SectionData::Read<Elf>(diag, AsBytes(kDataWithOffset));
    ASSERT_TRUE(read);
    EXPECT_EQ(read->contents().size_bytes(), 5u);
    auto offset = read->template read_offset<Elf>(1);
    EXPECT_THAT(offset, ::testing::Optional(::testing::Eq(1234u)));
  }
  {
    struct [[gnu::packed]] DataWithOffset {
      std::byte extra_byte{23};
      [[gnu::packed]] typename Elf::Xword offset{1234567890};
    };
    constexpr TestData<InitialLength64<Elf>, DataWithOffset> kDataWithOffset;
    auto read = elfldltl::dwarf::SectionData::Read<Elf>(diag, AsBytes(kDataWithOffset));
    ASSERT_TRUE(read);
    EXPECT_EQ(read->contents().size_bytes(), 9u);
    auto offset = read->template read_offset<Elf>(1);
    EXPECT_THAT(offset, ::testing::Optional(::testing::Eq(1234567890u)));
  }
}

TYPED_TEST(ElfldltlDwarfTests, SectionDataReadFail) {
  using Elf = typename TestFixture::Elf;

  // Empty buffer.
  {
    elfldltl::testing::ExpectedSingleError expected{
        "data size ",
        0,
        " too small for DWARF header",
        kErrorArg,
    };
    auto read = elfldltl::dwarf::SectionData::Read<Elf>(expected, kNoBytes, kErrorArg);
    EXPECT_FALSE(read);
  }

  // Buffer too short for any initial length.
  {
    constexpr std::array<std::byte, 3> kTooShortForHeader = {};
    elfldltl::testing::ExpectedSingleError expected{
        "data size ",
        kTooShortForHeader.size(),
        " too small for DWARF header",
        kErrorArg,
    };
    auto read =
        elfldltl::dwarf::SectionData::Read<Elf>(expected, AsBytes(kTooShortForHeader), kErrorArg);
    EXPECT_FALSE(read);
  }

  // Buffer too short for 64-bit initial length.
  {
    constexpr std::array<std::byte, 8> kTooShortForHeader64 = {
        std::byte{0xff},
        std::byte{0xff},
        std::byte{0xff},
        std::byte{0xff},
    };
    elfldltl::testing::ExpectedSingleError expected{
        "data size ",
        kTooShortForHeader64.size(),
        " too small for DWARF header",
        kErrorArg,
    };
    auto read =
        elfldltl::dwarf::SectionData::Read<Elf>(expected, AsBytes(kTooShortForHeader64), kErrorArg);
    EXPECT_FALSE(read);
  }

  // Reserved initial length values.
  for (uint32_t value = elfldltl::dwarf::kDwarf32Limit; value < elfldltl::dwarf::kDwarf64Length;
       ++value) {
    const InitialLength32<Elf> kReserved{value};
    elfldltl::testing::ExpectedSingleError expected{
        "Reserved initial-length value ",
        value,
        " used in DWARF header",
        kErrorArg,
    };
    auto read = elfldltl::dwarf::SectionData::Read<Elf>(expected, AsBytes(kReserved), kErrorArg);
    EXPECT_FALSE(read);
  }

  // Truncated contents for 32-bit length.
  {
    constexpr InitialLength32<Elf> kTooShortForLength{1};
    elfldltl::testing::ExpectedSingleError expected{
        "data size ", 4, " < ", 5, " required by DWARF header", kErrorArg,
    };
    auto read =
        elfldltl::dwarf::SectionData::Read<Elf>(expected, AsBytes(kTooShortForLength), kErrorArg);
    EXPECT_FALSE(read);
  }

  // Truncated contents for 64-bit length.
  {
    constexpr InitialLength64<Elf> kTooShortForLength64{1};
    elfldltl::testing::ExpectedSingleError expected{
        "data size ", 12, " < ", 13, " required by DWARF header", kErrorArg,
    };
    auto read =
        elfldltl::dwarf::SectionData::Read<Elf>(expected, AsBytes(kTooShortForLength64), kErrorArg);
    EXPECT_FALSE(read);
  }
}

}  // namespace
