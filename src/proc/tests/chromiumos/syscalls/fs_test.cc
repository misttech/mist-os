// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>

#include <algorithm>
#include <climits>
#include <cstdint>
#include <string>
#include <vector>

#include <gtest/gtest.h>

#include "src/proc/tests/chromiumos/syscalls/test_helper.h"

namespace {

std::vector<std::string> GetEntries(DIR *d) {
  std::vector<std::string> entries;

  struct dirent *entry;
  while ((entry = readdir(d)) != nullptr) {
    entries.push_back(entry->d_name);
  }
  return entries;
}

TEST(FsTest, NoDuplicatedDoDirectories) {
  DIR *root_dir = opendir("/");
  std::vector<std::string> entries = GetEntries(root_dir);
  std::vector<std::string> dot_entries;
  std::copy_if(entries.begin(), entries.end(), std::back_inserter(dot_entries),
               [](const std::string &filename) { return filename == "." || filename == ".."; });
  closedir(root_dir);

  ASSERT_EQ(2u, dot_entries.size());
  ASSERT_NE(dot_entries[0], dot_entries[1]);
}

TEST(FsTest, ReadDirRespectsSeek) {
  DIR *root_dir = opendir("/");
  std::vector<std::string> entries = GetEntries(root_dir);
  closedir(root_dir);

  root_dir = opendir("/");
  readdir(root_dir);
  long position = telldir(root_dir);
  closedir(root_dir);
  root_dir = opendir("/");
  seekdir(root_dir, position);
  std::vector<std::string> next_entries = GetEntries(root_dir);
  closedir(root_dir);

  EXPECT_NE(next_entries[0], entries[0]);
  EXPECT_LT(next_entries.size(), entries.size());
  // Remove the first elements from entries
  entries.erase(entries.begin(), entries.begin() + (entries.size() - next_entries.size()));
  EXPECT_EQ(entries, next_entries);
}

TEST(FsTest, FchmodTest) {
  char *tmp = getenv("TEST_TMPDIR");
  std::string path = tmp == nullptr ? "/tmp/fchmodtest" : std::string(tmp) + "/fchmodtest";
  int fd = open(path.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0777);
  ASSERT_GE(fd, 0);
  ASSERT_EQ(fchmod(fd, S_IRWXU | S_IRWXG), 0);
  ASSERT_EQ(fchmod(fd, S_IRWXU | S_IRWXG | S_IFCHR), 0);
}

TEST(FsTest, DevZeroAndNullQuirks) {
  size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGESIZE));

  for (const auto path : {"/dev/zero", "/dev/null"}) {
    SCOPED_TRACE(path);
    int fd = open(path, O_RDWR);

    // Attempting to write with an invalid buffer pointer still successfully "writes" the specified
    // number of bytes.
    EXPECT_EQ(write(fd, NULL, page_size), static_cast<ssize_t>(page_size));

    // write will report success up to the maximum number of bytes.
    ssize_t max_rw_count = 0x8000'0000 - page_size;
    EXPECT_EQ(write(fd, NULL, max_rw_count), max_rw_count);

    // Attempting to write more than this reports a short write.
    EXPECT_EQ(write(fd, NULL, max_rw_count + 1), max_rw_count);

    // Producing a range that goes outside the userspace accessible range does produce EFAULT.
    ssize_t implausibly_large_len = (1ll << 48);

    EXPECT_EQ(write(fd, NULL, implausibly_large_len), -1);
    EXPECT_EQ(errno, EFAULT);

    // A pointer unlikely to be backed by real memory is successful.
    void *plausible_pointer = reinterpret_cast<void *>(1ll << 30);
    EXPECT_EQ(write(fd, plausible_pointer, 1), 1);

    // An implausible pointer is unsuccessful.
    void *implausible_pointer = reinterpret_cast<void *>(implausibly_large_len);
    EXPECT_EQ(write(fd, implausible_pointer, 1), -1);
    EXPECT_EQ(errno, EFAULT);

    // Passing an invalid iov pointer produces EFAULT.
    EXPECT_EQ(writev(fd, NULL, 1), -1);
    EXPECT_EQ(errno, EFAULT);

    struct iovec iov_null_base_valid_length[] = {{
        .iov_base = NULL,
        .iov_len = 1,
    }};

    // Passing a valid iov pointer with null base pointers "successfully" writes the number of bytes
    // specified in the entry.
    EXPECT_EQ(writev(fd, iov_null_base_valid_length, 1), 1);

    struct iovec iov_null_base_max_rw_count_length[] = {{
        .iov_base = NULL,
        .iov_len = static_cast<size_t>(max_rw_count),
    }};
    EXPECT_EQ(writev(fd, iov_null_base_max_rw_count_length, 1), max_rw_count);

    struct iovec iov_null_base_max_rw_count_in_two_entries[] = {
        {
            .iov_base = NULL,
            .iov_len = static_cast<size_t>(max_rw_count - 100),
        },
        {
            .iov_base = NULL,
            .iov_len = 100,
        },
    };
    EXPECT_EQ(writev(fd, iov_null_base_max_rw_count_in_two_entries, 2), max_rw_count);

    struct iovec iov_null_base_max_rwcount_length_plus_one[] = {{
        .iov_base = NULL,
        .iov_len = static_cast<size_t>(max_rw_count + 1),
    }};
    EXPECT_EQ(writev(fd, iov_null_base_max_rwcount_length_plus_one, 1), max_rw_count);

    struct iovec iov_null_base_max_rwcount_length_plus_one_in_two_entries[] = {
        {
            .iov_base = NULL,
            .iov_len = static_cast<size_t>(max_rw_count - 100),
        },
        {
            .iov_base = NULL,
            .iov_len = 101,
        },
    };
    EXPECT_EQ(writev(fd, iov_null_base_max_rwcount_length_plus_one_in_two_entries, 2),
              max_rw_count);

    // Implausibly large iov_len values still generate EFAULT.
    struct iovec iov_null_base_implausible_length[] = {{
        .iov_base = NULL,
        .iov_len = static_cast<size_t>(implausibly_large_len),
    }};
    EXPECT_EQ(writev(fd, iov_null_base_implausible_length, 1), -1);
    EXPECT_EQ(errno, EFAULT);

    struct iovec iov_null_base_implausible_length_behind_max_rw_count[] = {
        {
            .iov_base = NULL,
            .iov_len = static_cast<size_t>(max_rw_count),
        },
        {
            .iov_base = NULL,
            .iov_len = static_cast<size_t>(implausibly_large_len),
        },
    };

    EXPECT_EQ(writev(fd, iov_null_base_implausible_length_behind_max_rw_count, 2), -1);
    EXPECT_EQ(errno, EFAULT);

    if (std::string(path) == "/dev/null") {
      // Reading any plausible number of bytes from an invalid buffer pointer into /dev/null
      // will successfully read 0 bytes.
      EXPECT_EQ(read(fd, NULL, 1), 0);
      EXPECT_EQ(read(fd, NULL, max_rw_count), 0);
      EXPECT_EQ(read(fd, NULL, max_rw_count + 1), 0);
    }

    // Reading an implausibly large number of bytes from /dev/zero or /dev/null will fail with
    // EFAULT.
    EXPECT_EQ(read(fd, NULL, implausibly_large_len), -1);
    EXPECT_EQ(errno, EFAULT);

    close(fd);
  }
}

}  // namespace
