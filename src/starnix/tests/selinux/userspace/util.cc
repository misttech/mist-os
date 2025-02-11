// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/selinux/userspace/util.h"

#include <fcntl.h>
#include <lib/fit/defer.h>
#include <string.h>
#include <sys/mman.h>

#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

void WriteContents(const std::string& file, const std::string& contents, bool create) {
  auto fd = fbl::unique_fd(open(file.c_str(), O_WRONLY | (create ? O_CREAT : 0), 0777));
  ASSERT_TRUE(fd) << "Opening " << file << " for writing: " << strerror(errno);

  ASSERT_EQ(write(fd.get(), contents.data(), contents.size()), ssize_t(contents.size()))
      << strerror(errno);
}

void LoadPolicy(const std::string& name) {
  auto fd = fbl::unique_fd(open(("data/policies/" + name).c_str(), O_RDONLY));
  ASSERT_TRUE(fd) << "Opening policy " << name << ": " << strerror(errno);

  off_t fsize;
  ASSERT_SUCCESS(fsize = lseek(fd.get(), 0, SEEK_END));

  void* address = mmap(NULL, (size_t)fsize, PROT_READ, MAP_PRIVATE, fd.get(), 0);
  ASSERT_NE(address, MAP_FAILED) << strerror(errno);
  auto unmap = fit::defer([address, fsize] { ASSERT_SUCCESS(munmap(address, fsize)); });

  auto policy_load_fd = fbl::unique_fd(open("/sys/fs/selinux/load", O_WRONLY));
  ASSERT_TRUE(policy_load_fd) << strerror(errno);

  ssize_t written = write(policy_load_fd.get(), address, fsize);
  ASSERT_EQ(written, ssize_t(fsize)) << "writing policy: " << strerror(errno);
}

std::string ReadFile(const std::string& name) {
  auto fd = fbl::unique_fd(open(name.c_str(), O_RDONLY));
  EXPECT_TRUE(fd) << "Opening " << name << ": " << strerror(errno);

  if (!fd) {
    return "";
  }

  std::string contents;
  char buf[4096];
  ssize_t read_len;
  while ((read_len = read(fd.get(), buf, sizeof(buf))) > 0) {
    contents.append(buf, read_len);
  }
  EXPECT_NE(read_len, -1) << "Error reading: " << strerror(errno);
  return contents;
}
