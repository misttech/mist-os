// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/tty.h>

#include <zxtest/zxtest.h>

#include "lib/stdcompat/array.h"

namespace {

TEST(TtyFromCmdlineTest, NoEntryReturnsTty0) {
  auto tty = boot_shim::TtyFromCmdline("");
  ASSERT_TRUE(tty);
  EXPECT_EQ(tty->type, boot_shim::TtyType::kAny);
  EXPECT_EQ(tty->index, 0);
}

TEST(TtyFromCmdlineTest, SerialEntryWithIndexIsTtySN) {
  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyS1234 foobar");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kSerial);
    EXPECT_EQ(tty->index, 1234);
  }

  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyS1234 ");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kSerial);
    EXPECT_EQ(tty->index, 1234);
  }

  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyS1234");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kSerial);
    EXPECT_EQ(tty->index, 1234);
  }
}

TEST(TtyFromCmdlineTest, EntryWithIndexIsTtyN) {
  {
    auto tty = boot_shim::TtyFromCmdline("foo console=tty1234 foobar");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kAny);
    EXPECT_EQ(tty->index, 1234);
  }

  {
    auto tty = boot_shim::TtyFromCmdline("foo console=tty1234 ");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kAny);
    EXPECT_EQ(tty->index, 1234);
  }

  {
    auto tty = boot_shim::TtyFromCmdline("foo console=tty1234");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kAny);
    EXPECT_EQ(tty->index, 1234);
  }
}

TEST(TtyFromCmdlineTest, AmlEntryWithIndexIsTtyAMLN) {
  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyAML1234 foobar");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kAml);
    EXPECT_EQ(tty->index, 1234);
  }

  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyAML1234 ");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kAml);
    EXPECT_EQ(tty->index, 1234);
  }

  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyAML12345");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kAml);
    EXPECT_EQ(tty->index, 12345);
  }
}

TEST(TtyFromCmdlineTest, AmlEntryWithIndexIsTtyMSMN) {
  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyMSM1234 foobar");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kMsm);
    EXPECT_EQ(tty->index, 1234);
  }

  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyMSM1234 ");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kMsm);
    EXPECT_EQ(tty->index, 1234);
  }

  {
    auto tty = boot_shim::TtyFromCmdline("foo console=ttyMSM12345");
    ASSERT_TRUE(tty);
    EXPECT_EQ(tty->type, boot_shim::TtyType::kMsm);
    EXPECT_EQ(tty->index, 12345);
  }
}

TEST(TtyFromCmdlineTest, InvalidArgumentIsNullOpt) {
  constexpr auto kInvalidArgs = cpp20::to_array({
      "console=tyS123",
      "console=ty123",
      "console=123",
      "console=S123",
      "console=AML123",
      "console=tty",
      "console=ttyS",
      "console=ttyAML",
      "console=ttyFOO",
      "console=foo",
  });

  for (std::string_view arg : kInvalidArgs) {
    EXPECT_TRUE(!boot_shim::TtyFromCmdline(arg).has_value(), "ARG: %.*s",
                static_cast<int>(arg.size()), arg.data());
  }
}

}  // namespace
