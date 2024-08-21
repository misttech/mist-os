// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/open_flags.h>

#include <zxtest/zxtest.h>

namespace {

using namespace starnix_uapi;

TEST(OpenFlags, test_access) {
  auto read_only = OpenFlagsImpl::from_bits_truncate(O_RDONLY);
  EXPECT_TRUE(read_only.can_read());
  EXPECT_FALSE(read_only.can_write());

  auto write_only = OpenFlagsImpl::from_bits_truncate(O_WRONLY);
  EXPECT_FALSE(write_only.can_read());
  EXPECT_TRUE(write_only.can_write());

  auto read_write = OpenFlagsImpl::from_bits_truncate(O_RDWR);
  EXPECT_TRUE(read_write.can_read());
  EXPECT_TRUE(read_write.can_write());

  EXPECT_TRUE(static_cast<uint64_t>(OpenFlagsImpl::EnumType::ACCESS_MASK) == 3ul);
}
// using namespace starnix_uapi;
}  // namespace
