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
  ASSERT_THAT(fd.get(), SyscallSucceeds()) << "while opening file for writing: " << file;

  ssize_t written = write(fd.get(), contents.data(), contents.size());
  ASSERT_THAT(written, SyscallSucceeds());
  EXPECT_EQ(size_t(written), contents.size()) << "short write in " << file;
}

void LoadPolicy(const std::string& name) {
  auto fd = fbl::unique_fd(open(("data/policies/" + name).c_str(), O_RDONLY));
  ASSERT_THAT(fd.get(), SyscallSucceeds()) << "opening policy at: " << name;

  off_t fsize;
  ASSERT_THAT((fsize = lseek(fd.get(), 0, SEEK_END)), SyscallSucceeds());

  void* address = mmap(NULL, (size_t)fsize, PROT_READ, MAP_PRIVATE, fd.get(), 0);
  ASSERT_THAT(reinterpret_cast<intptr_t>(address), SyscallSucceeds());
  auto unmap =
      fit::defer([address, fsize] { ASSERT_THAT(munmap(address, fsize), SyscallSucceeds()); });

  WriteContents("/sys/fs/selinux/load", std::string(reinterpret_cast<char*>(address), fsize));
}

std::string ReadFile(const std::string& name) {
  auto fd = fbl::unique_fd(open(name.c_str(), O_RDONLY));
  EXPECT_THAT(fd.get(), SyscallSucceeds()) << "while opening file for reading: " << name;
  if (!fd) {
    return "";
  }

  std::string contents;
  char buf[4096];
  ssize_t read_len;
  while ((read_len = read(fd.get(), buf, sizeof(buf))) > 0) {
    contents.append(buf, read_len);
  }
  EXPECT_THAT(read_len, SyscallSucceeds()) << "error reading " << name;
  return contents;
}
