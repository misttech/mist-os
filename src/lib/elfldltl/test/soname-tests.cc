// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/gnu-hash.h>
#include <lib/elfldltl/soname.h>

#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include <gtest/gtest.h>

namespace {

constexpr std::string_view kOther = "other";
constexpr uint32_t kOtherHash = elfldltl::GnuHashString(kOther);

TEST(ElfldltlSonameTests, Basic) {
  elfldltl::Soname name{"test"};
  EXPECT_EQ(name.str(), "test");

  elfldltl::Soname other{"other"};
  EXPECT_EQ(other.str(), "other");

  name = other;
  EXPECT_EQ(name.str(), "other");
  EXPECT_EQ(other, name);
  EXPECT_EQ(other.hash(), kOtherHash);

  // Test assignment to std::string_view.
  name = kOther;
  EXPECT_EQ(name.str(), kOther);
  EXPECT_EQ(other, name);
  EXPECT_EQ(other.hash(), kOtherHash);

  // Test converting assignment to std::string_view.
  name = "other";
  EXPECT_EQ(name.str(), kOther);
  EXPECT_EQ(other, name);
  EXPECT_EQ(other.hash(), kOtherHash);

  elfldltl::Soname a{"a"}, b{"b"};
  EXPECT_LT(a, b);
  EXPECT_LE(a, b);
  EXPECT_LE(a, a);
  EXPECT_GT(b, a);
  EXPECT_GE(b, a);
  EXPECT_GE(a, a);
  EXPECT_EQ(a, a);
  EXPECT_NE(a, b);
}

TEST(ElfldltlSonameTests, Remote) {
  using RemoteSoname = elfldltl::Soname<elfldltl::Elf<>, elfldltl::RemoteAbiTraits>;

  RemoteSoname name;
  name = RemoteSoname(name);
  EXPECT_EQ(name.hash(), 0u);
}

}  // anonymous namespace
