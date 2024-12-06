// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/vfs/fs_args.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

#include <ktl/pair.h>

namespace unit_testing {

using starnix::FsStr;
using starnix::FsStringHashTable;
using starnix::MountParams;
using starnix_uapi::MountFlags;
using starnix_uapi::MountFlagsEnum;

namespace {
bool empty_data() {
  BEGIN_TEST;

  auto parsed_data = MountParams::parse(FsStr()).value();
  ASSERT_TRUE(parsed_data.is_empty());

  END_TEST;
}
}  // namespace

bool parse_options_with_trailing_comma() {
  BEGIN_TEST;

  FsStr data{"key0=value0,"};
  auto parsed_data = MountParams::parse(data);
  ASSERT_TRUE(parsed_data.is_ok(), "mount options parse:  key0=value0,");

  ktl::string_view expected("value0");
  ktl::string_view actual = parsed_data->options_.find("key0")->value;
  ASSERT_STREQ(expected.data(), actual.data());

  END_TEST;
}

bool parse_options_last_value_wins() {
  BEGIN_TEST;

  FsStr data{"key0=value0,key1,key2=value2,key0=value3"};
  auto parsed_data = MountParams::parse(data);
  ASSERT_TRUE(parsed_data.is_ok(),
              "mount options parse:  key0=value0,key1,key2=value2,key0=value3");

  ktl::string_view expected;
  ktl::string_view actual = parsed_data->options_.find("key1")->value;
  ASSERT_STREQ(expected.data(), actual.data());

  expected = ktl::string_view("value2");
  actual = parsed_data->options_.find("key2")->value;
  ASSERT_STREQ(expected.data(), actual.data());

  expected = ktl::string_view("value3");
  actual = parsed_data->options_.find("key0")->value;
  ASSERT_STREQ(expected.data(), actual.data());

  END_TEST;
}

bool parse_options_quoted() {
  BEGIN_TEST;

  FsStr data{"key0=unqouted,key1=\"quoted,with=punc:tua-tion.\""};
  auto parsed_data = MountParams::parse(data);
  ASSERT_TRUE(parsed_data.is_ok(),
              "mount options parse:  key0=unqouted,key1=\"quoted,with=punc:tua-tion.\"");

  ktl::string_view expected("unqouted");
  ktl::string_view actual = parsed_data->options_.find("key0")->value;
  ASSERT_STREQ(expected.data(), actual.data());

  expected = ktl::string_view("quoted,with=punc:tua-tion.");
  actual = parsed_data->options_.find("key1")->value;
  ASSERT_STREQ(expected.data(), actual.data());

  END_TEST;
}

bool parse_options_misquoted() {
  BEGIN_TEST;

  FsStr data{"key0=\"mis\"quoted,key1=\"quoted\""};
  auto parsed_data = MountParams::parse(data);
  ASSERT_FALSE(parsed_data.is_ok(), "mount options parse:  key0=\"mis\"quoted,key1=\"quoted\"");

  END_TEST;
}

bool parse_options_misquoted_tail() {
  BEGIN_TEST;

  FsStr data{"key0=\"quoted\",key1=\"mis\"quoted"};
  auto parsed_data = MountParams::parse(data);
  ASSERT_FALSE(parsed_data.is_ok(), "mount options parse:  key0=\"quoted\",key1=\"mis\"quoted");

  END_TEST;
}

bool parse_normal_mount_flags() {
  BEGIN_TEST;

  FsStr data{"nosuid,nodev,noexec,relatime"};
  auto parsed_data = MountParams::parse(data);
  ASSERT_TRUE(parsed_data.is_ok(), "mount options parse: nosuid,nodev,noexec,relatime");

  auto expected_options = {ktl::pair("nosuid", ""), ktl::pair("nodev", ""), ktl::pair("noexec", ""),
                           ktl::pair("relatime", "")};

  for (const auto& [key, value] : expected_options) {
    ASSERT_TRUE(parsed_data->options_.find(key) != parsed_data->options_.end());
    ASSERT_STREQ(value, parsed_data->options_.find(key)->value.data());
  }

  END_TEST;
}

bool parse_and_remove_normal_mount_flags() {
  BEGIN_TEST;

  FsStr data{"nosuid,nodev,noexec,relatime"};
  auto parsed_data = MountParams::parse(data);
  ASSERT_TRUE(parsed_data.is_ok(), "mount options parse: nosuid,nodev,noexec,relatime");

  auto flags = parsed_data->remove_mount_flags();
  ASSERT_EQ((MountFlags(MountFlagsEnum::NOSUID) | MountFlagsEnum::NODEV | MountFlagsEnum::NOEXEC |
             MountFlagsEnum::RELATIME)
                .bits(),
            flags.bits());

  END_TEST;
}

namespace {
bool parse_data() {
  BEGIN_TEST;
  ASSERT_EQ(42u, starnix::parse<size_t>("42").value());
  END_TEST;
}
}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_args)
UNITTEST("empty data", unit_testing::empty_data)
UNITTEST("parse options with trailing comma", unit_testing::parse_options_with_trailing_comma)
UNITTEST("parse options last value wins", unit_testing::parse_options_last_value_wins)
UNITTEST("parse options quoted", unit_testing::parse_options_quoted)
UNITTEST("parse options misquoted", unit_testing::parse_options_misquoted)
UNITTEST("parse options misquoted tail", unit_testing::parse_options_misquoted_tail)
UNITTEST("parse normal mount flags", unit_testing::parse_normal_mount_flags)
UNITTEST("parse and remove normal mount flags", unit_testing::parse_and_remove_normal_mount_flags)
UNITTEST("parse data", unit_testing::parse_data)
UNITTEST_END_TESTCASE(starnix_fs_args, "starnix_fs_args", "Tests for FsArgs")
