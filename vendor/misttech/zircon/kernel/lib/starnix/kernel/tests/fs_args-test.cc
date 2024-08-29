// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/vfs/fs_args.h>

#include <zxtest/zxtest.h>

namespace {

using namespace starnix;

TEST(FsArgs, empty_data) {
  FsStr empty;
  fbl::HashTable<FsString, ktl::unique_ptr<fs_args::HashableFsString>> parsed_data;
  fs_args::generic_parse_mount_options(empty, &parsed_data);

  ASSERT_TRUE(parsed_data.is_empty());
}

TEST(FsArgs, parse_options) {
  FsStr data{"key0=value0,key1,key2=value2,key0=value3"};
  fbl::HashTable<FsString, ktl::unique_ptr<fs_args::HashableFsString>> parsed_data;
  fs_args::generic_parse_mount_options(data, &parsed_data);

  ASSERT_EQ(parsed_data.find("key1")->value, "");
  ASSERT_EQ(parsed_data.find("key2")->value, "value2");
  ASSERT_EQ(parsed_data.find("key0")->value, "value3");
}

TEST(FsArgs, parse_data) { ASSERT_EQ(42u, fs_args::parse<size_t>("42")); }

}  // namespace
