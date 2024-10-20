// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>

#include <gtest/gtest.h>

#include "src/storage/host_fs_test/fixture.h"
#include "src/storage/minfs/host.h"

namespace fs_test {
namespace {

TEST_F(HostFilesystemTest, Basic) {
  ASSERT_EQ(emu_mkdir("::alpha"), 0);
  ASSERT_EQ(emu_mkdir("::alpha/bravo"), 0);
  ASSERT_EQ(emu_mkdir("::alpha/bravo/charlie"), 0);
  ASSERT_EQ(emu_mkdir("::alpha/bravo/charlie/delta"), 0);
  ASSERT_EQ(emu_mkdir("::alpha/bravo/charlie/delta/echo"), 0);
  int fd1 = emu_open("::alpha/bravo/charlie/delta/echo/foxtrot", O_RDWR | O_CREAT);
  ASSERT_GT(fd1, 0);
  int fd2 = emu_open("::alpha/bravo/charlie/delta/echo/foxtrot", O_RDWR);
  ASSERT_GT(fd2, 0);
  ASSERT_EQ(emu_write(fd1, "Hello, World!\n", 14), 14);
  ASSERT_EQ(emu_close(fd1), 0);
  ASSERT_EQ(emu_close(fd2), 0);

  fd1 = emu_open("::file.txt", O_CREAT | O_RDWR);
  ASSERT_GT(fd1, 0);
  ASSERT_EQ(emu_close(fd1), 0);

  ASSERT_EQ(emu_mkdir("::emptydir"), 0);
  fd1 = emu_open("::emptydir", O_RDONLY);
  ASSERT_GT(fd1, 0);
  char buf;
  ASSERT_LT(emu_read(fd1, &buf, 1), 0);
  ASSERT_EQ(emu_write(fd1, "Don't write to directories", 26), -1);
  ASSERT_EQ(emu_ftruncate(fd1, 0), -1);
  ASSERT_EQ(emu_close(fd1), 0);
  ASSERT_EQ(RunFsck(), 0);
}

}  // namespace
}  // namespace fs_test
