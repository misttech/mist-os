// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fxt/interned_category.h>

#include <set>

#include <gtest/gtest.h>

namespace {

using fxt::operator""_category;

uint32_t RegisterInternedCategory(const fxt::InternedCategory& interned_category) {
  static uint32_t next_category_bit{1};

  uint32_t bit_number = interned_category.bit_number;
  if (bit_number == fxt::InternedCategory::kInvalidBitNumber) {
    interned_category.bit_number = bit_number = next_category_bit++;
  }
  return bit_number;
}

TEST(Types, InternedCategory) {
  const fxt::InternedCategory& foo = "foo"_category;
  const fxt::InternedCategory& bar = "bar"_category;
  const fxt::InternedCategory& foo2 = "foo"_category;

  EXPECT_EQ(&foo, &foo2);
  EXPECT_NE(&foo, &bar);

  EXPECT_EQ(&foo.label, &foo2.label);
  EXPECT_NE(&foo.label, &bar.label);

  EXPECT_STREQ(foo.string(), "foo");
  EXPECT_STREQ(bar.string(), "bar");

  EXPECT_EQ(foo.bit_number, fxt::InternedCategory::kInvalidBitNumber);
  EXPECT_EQ(bar.bit_number, fxt::InternedCategory::kInvalidBitNumber);

  fxt::InternedCategory::SetRegisterCategoryCallback(RegisterInternedCategory);

  std::set<const fxt::InternedCategory*> target_category_set{&foo, &bar};

  // Iterate over the section containing category instances. This is only populated on supported
  // compilers. Interned categories from other tests in the same test binary will show up here if
  // any show up at all.
  std::set<const fxt::InternedCategory*> section_category_set{};
  for (const fxt::InternedCategory& category : fxt::InternedCategory::IterateSection) {
    section_category_set.emplace(&category);
  }
  if (!section_category_set.empty()) {
    for (const fxt::InternedCategory* string : target_category_set) {
      EXPECT_EQ(1u, section_category_set.count(string));
    }
  }

  // Attempt to register the interned categories using the linker section.
  fxt::InternedCategory::PreRegister();

  // Manually register the interned categories on unsupported compilers.
  // TODO(https://fxbug.dev/42108473): Remove when GCC supports COMDAT section attributes.
#ifndef __clang__
  EXPECT_NE(foo.GetBit(), fxt::InternedCategory::kInvalidBitNumber);
  EXPECT_NE(bar.GetBit(), fxt::InternedCategory::kInvalidBitNumber);
#endif

  EXPECT_NE(foo.bit_number, fxt::InternedCategory::kInvalidBitNumber);
  EXPECT_NE(bar.bit_number, fxt::InternedCategory::kInvalidBitNumber);

  EXPECT_NE(bar.bit_number, foo.bit_number);

  // Iterate over the list containing category instances. This should contain all the instances that
  // have had GetId() called at least once OR have been successfully pre-registered. Interned
  // categories from other tests in the same test binary may show up here.
  std::set<const fxt::InternedCategory*> list_category_set{};
  for (const fxt::InternedCategory& category : fxt::InternedCategory::IterateList) {
    list_category_set.emplace(&category);
  }
  for (const fxt::InternedCategory* category : target_category_set) {
    EXPECT_EQ(1u, list_category_set.count(category));
  }
}

}  // anonymous namespace
