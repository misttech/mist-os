// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fxt/interned_string.h>

#include <set>

#include <gtest/gtest.h>

namespace {

using fxt::operator""_intern;

TEST(Types, InternedString) {
  const fxt::InternedString& foo = "foo"_intern;
  const fxt::InternedString& bar = "bar"_intern;
  const fxt::InternedString& foo2 = "foo"_intern;

  EXPECT_EQ(&foo, &foo2);
  EXPECT_NE(&bar, &foo);

  EXPECT_STREQ(foo.string, "foo");
  EXPECT_STREQ(bar.string, "bar");

  EXPECT_EQ(foo.id, fxt::InternedString::kInvalidId);
  EXPECT_EQ(bar.id, fxt::InternedString::kInvalidId);

  const std::set<const fxt::InternedString*> target_string_set{&foo, &bar};

  // Iterate over the section containing string instances. This is only populated on supported
  // compilers. Interned strings from other tests in the same test binary will show up here if any
  // show up at all.
  std::set<const fxt::InternedString*> section_string_set{};
  for (const fxt::InternedString& string : fxt::InternedString::IterateSection) {
    section_string_set.emplace(&string);
  }
  if (!section_string_set.empty()) {
    for (const fxt::InternedString* string : target_string_set) {
      EXPECT_EQ(1u, section_string_set.count(string));
    }
  }

  // Attempt to register the interned strings using the linker section.
  fxt::InternedString::PreRegister();

  // Manually register the interned strings on unsupported compilers.
  // TODO(https://fxbug.dev/33293): Remove when GCC supports COMDAT section attributes.
#ifndef __clang__
  EXPECT_NE(foo.GetId(), fxt::InternedString::kInvalidId);
  EXPECT_NE(bar.GetId(), fxt::InternedString::kInvalidId);
#endif

  EXPECT_NE(foo.id, fxt::InternedString::kInvalidId);
  EXPECT_NE(bar.id, fxt::InternedString::kInvalidId);

  EXPECT_NE(bar.id, foo.id);

  // Iterate over the list containing string instances. This should contain all the instances that
  // have had GetId() called at least once OR have been successfully pre-registered. Interned
  // strings from other tests in the same test binary may show up here.
  std::set<const fxt::InternedString*> list_string_set{};
  for (const fxt::InternedString& string : fxt::InternedString::IterateList) {
    list_string_set.emplace(&string);
  }
  for (const fxt::InternedString* string : target_string_set) {
    EXPECT_EQ(1u, list_string_set.count(string));
  }
}

}  // anonymous namespace
