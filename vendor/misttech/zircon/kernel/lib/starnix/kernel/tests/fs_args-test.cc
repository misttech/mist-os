// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/vfs/fs_args.h>
#include <lib/unittest/unittest.h>

using namespace starnix;

bool fs_args_empty_data() {
  BEGIN_TEST;
  FsStr empty;
  fbl::HashTable<ktl::string_view, ktl::unique_ptr<fs_args::HashableFsString>> parsed_data;
  fs_args::generic_parse_mount_options(empty, &parsed_data);

  ASSERT_TRUE(parsed_data.is_empty());

  END_TEST;
}

bool fs_args_parse_options_last_value_wins() {
  BEGIN_TEST;
  FsStr data{"key0=value0,key1,key2=value2,key0=value3"};
  fbl::HashTable<ktl::string_view, ktl::unique_ptr<fs_args::HashableFsString>> parsed_data;
  fs_args::generic_parse_mount_options(data, &parsed_data);

  ktl::string_view expected("");
  ktl::string_view actual = parsed_data.find("key1")->value;
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  expected = ktl::string_view("value2");
  actual = parsed_data.find("key2")->value;
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  expected = ktl::string_view("value3");
  actual = parsed_data.find("key0")->value;
  ASSERT_EQ(expected.size(), actual.size());
  ASSERT_BYTES_EQ(reinterpret_cast<const uint8_t*>(expected.data()),
                  reinterpret_cast<const uint8_t*>(actual.data()), expected.size());

  END_TEST;
}

bool fs_args_parse_data() {
  BEGIN_TEST;
  ASSERT_EQ(42u, fs_args::parse<size_t>("42").value());
  END_TEST;
}

UNITTEST_START_TESTCASE(starnix_fs_args)
UNITTEST("test empty data", fs_args_empty_data)
UNITTEST("test parse last value wins", fs_args_parse_options_last_value_wins)
UNITTEST("test parse data", fs_args_parse_data)

UNITTEST_END_TESTCASE(starnix_fs_args, "starnix_fs_args", "Tests for FsArgs")
