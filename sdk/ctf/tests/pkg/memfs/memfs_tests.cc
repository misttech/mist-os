// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/memfs/memfs.h>
#include <sys/stat.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

namespace {

TEST(MemfsTests, TestMemfsInstall) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  ASSERT_EQ(loop.StartThread(), ZX_OK);

  memfs_filesystem_t* fs;
  ASSERT_EQ(memfs_install_at(loop.dispatcher(), "/mytmp", &fs), ZX_OK);
  int fd = open("/mytmp", O_DIRECTORY | O_RDONLY);
  ASSERT_GE(fd, 0);

  // Access files within the filesystem.
  DIR* d = fdopendir(fd);

  // Create a file
  const char* filename = "file-a";
  fd = openat(dirfd(d), filename, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR);
  ASSERT_GE(fd, 0);
  const char* data = "hello";
  ssize_t datalen = strlen(data);
  ASSERT_EQ(write(fd, data, datalen), datalen);
  ASSERT_EQ(lseek(fd, 0, SEEK_SET), 0);
  char buf[32];
  ASSERT_EQ(read(fd, buf, sizeof(buf)), datalen);
  ASSERT_EQ(memcmp(buf, data, datalen), 0);
  close(fd);

  // Readdir the file
  struct dirent* de;
  ASSERT_NE((de = readdir(d)), nullptr);
  ASSERT_EQ(strcmp(de->d_name, "."), 0);
  ASSERT_NE((de = readdir(d)), nullptr);
  ASSERT_EQ(strcmp(de->d_name, filename), 0);
  ASSERT_EQ(readdir(d), nullptr);

  ASSERT_EQ(closedir(d), 0);
  memfs_filesystem_t* fs_2;
  ASSERT_EQ(memfs_install_at(loop.dispatcher(), "/mytmp", &fs_2), ZX_ERR_ALREADY_EXISTS);

  // Wait for cleanup of failed memfs install.
  sync_completion_t unmounted;
  async::PostTask(loop.dispatcher(), [&unmounted]() { sync_completion_signal(&unmounted); });
  ASSERT_EQ(sync_completion_wait(&unmounted, zx::duration::infinite().get()), ZX_OK);

  // Synchronously clean up the good one.
  sync_completion_reset(&unmounted);
  memfs_free_filesystem(fs, &unmounted);
  ASSERT_EQ(sync_completion_wait(&unmounted, zx::duration::infinite().get()), ZX_OK);

  loop.Shutdown();
}

}  // namespace
