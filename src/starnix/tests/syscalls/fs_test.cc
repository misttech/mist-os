// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>

#include <algorithm>
#include <climits>
#include <cstdint>
#include <string>
#include <vector>

#include <gtest/gtest.h>
#include <linux/capability.h>

#include "src/starnix/tests/syscalls/test_helper.h"

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

constexpr uid_t kOwnerUid = 65534;
constexpr uid_t kNonOwnerUid = 65533;
constexpr gid_t kOwnerGid = 65534;
constexpr gid_t kNonOwnerGid = 65533;

class UtimensatTest : public ::testing::Test {
 protected:
  void SetUp() {
    if (!test_helper::HasSysAdmin()) {
      GTEST_SKIP() << "Not running with sysadmin capabilities, skipping.";
    }

    char dir_template[] = "/tmp/XXXXXX";
    ASSERT_NE(mkdtemp(dir_template), nullptr)
        << "failed to create test folder: " << std::strerror(errno);
    test_folder_ = std::string(dir_template);

    test_file_ = test_folder_ + "/testfile";
    int fd = open(test_file_.c_str(), O_RDWR | O_CREAT, 0666);
    ASSERT_NE(fd, -1) << "failed to create test file: " << std::strerror(errno);
    close(fd);

    ASSERT_EQ(chown(test_folder_.c_str(), kOwnerUid, kOwnerGid), 0);
    ASSERT_EQ(chmod(test_folder_.c_str(), 0777), 0);
    ASSERT_EQ(chmod(test_file_.c_str(), 0666), 0);
    ASSERT_EQ(chown(test_file_.c_str(), kOwnerUid, kOwnerGid), 0);
  }

  void TearDown() {
    if (test_file_.length() != 0) {
      ASSERT_EQ(remove(test_file_.c_str()), 0);
    }
    if (test_folder_.length() != 0) {
      ASSERT_EQ(remove(test_folder_.c_str()), 0);
    }
  }

  // test folder owned by kOwnerUid, perms 0o777
  std::string test_folder_;

  // test file owned by kOwnerUid, perms 0o666
  std::string test_file_;
};

void unset_capability(int cap) {
  __user_cap_header_struct header;
  memset(&header, 0, sizeof(header));
  header.version = _LINUX_CAPABILITY_VERSION_3;
  __user_cap_data_struct caps[_LINUX_CAPABILITY_U32S_3];
  SAFE_SYSCALL(syscall(SYS_capget, &header, &caps));
  caps[CAP_TO_INDEX(cap)].effective &= ~CAP_TO_MASK(cap);
  SAFE_SYSCALL(syscall(SYS_capset, &header, &caps));
}

bool has_capability(int cap) {
  __user_cap_header_struct header;
  memset(&header, 0, sizeof(header));
  header.version = _LINUX_CAPABILITY_VERSION_3;
  __user_cap_data_struct caps[_LINUX_CAPABILITY_U32S_3];
  SAFE_SYSCALL(syscall(SYS_capget, &header, &caps));
  return caps[CAP_TO_INDEX(cap)].effective & CAP_TO_MASK(cap);
}

bool change_ids(uid_t user, gid_t group) {
  // TODO(https://fxbug.dev/125669): changing the filesystem user ID from 0 to
  // nonzero should drop capabilities, dropping them manually as a workaround.
  uid_t current_ruid, current_euid, current_suid;
  SAFE_SYSCALL(getresuid(&current_ruid, &current_euid, &current_suid));
  if (current_euid == 0 && user != 0) {
    unset_capability(CAP_DAC_OVERRIDE);
    unset_capability(CAP_FOWNER);
  }

  return (setresgid(group, group, group) == 0) && (setresuid(user, user, user) == 0);
}

TEST_F(UtimensatTest, OwnerCanAlwaysSetTime) {
  ASSERT_EQ(chmod(test_file_.c_str(), 0), 0);

  // File owner can change time to now even without write perms.
  ForkHelper helper;
  helper.RunInForkedProcess([this] {
    ASSERT_TRUE(change_ids(kOwnerUid, kOwnerGid));
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), NULL, 0))
        << "utimensat failed: " << std::strerror(errno);
  });

  EXPECT_TRUE(helper.WaitForChildren());

  // File owner can change time to any time without write perms.
  helper.RunInForkedProcess([this] {
    ASSERT_TRUE(change_ids(kOwnerUid, kOwnerGid));
    struct timespec times[2] = {{0, 0}};
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), times, 0))
        << "utimensat failed: " << std::strerror(errno);
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_F(UtimensatTest, NonOwnerWithWriteAccessCanOnlySetTimeToNow) {
  ASSERT_EQ(chmod(test_file_.c_str(), 0), 0);

  // Non file owner cannot change time to now without write perms.
  ForkHelper helper;
  helper.RunInForkedProcess([this] {
    ASSERT_TRUE(change_ids(kNonOwnerUid, kNonOwnerGid));
    EXPECT_NE(0, utimensat(-1, test_file_.c_str(), NULL, 0));
  });
  EXPECT_TRUE(helper.WaitForChildren());

  // Non file owner can change time to now with write perms.
  ASSERT_EQ(chmod(test_file_.c_str(), 0006), 0);
  helper.RunInForkedProcess([this] {
    ASSERT_TRUE(change_ids(kNonOwnerUid, kNonOwnerGid));
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), NULL, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  // Non file owner cannot change time to some other value, even with write
  // perms.
  helper.RunInForkedProcess([this] {
    ASSERT_TRUE(change_ids(kNonOwnerUid, kNonOwnerGid));
    struct timespec times[2] = {{0, 0}};
    EXPECT_NE(0, utimensat(-1, test_file_.c_str(), times, 0));
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_F(UtimensatTest, NonOwnerWithCapabilitiesCanSetTime) {
  ASSERT_EQ(chmod(test_file_.c_str(), 0), 0);

  // Non file owner without write permissions can set the time to now with
  // either CAP_DAC_OVERRIDE or CAP_FOWNER capability.
  ForkHelper helper;
  helper.RunInForkedProcess([this] {
    ASSERT_TRUE(has_capability(CAP_DAC_OVERRIDE));
    ASSERT_TRUE(has_capability(CAP_FOWNER));
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), NULL, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  helper.RunInForkedProcess([this] {
    unset_capability(CAP_DAC_OVERRIDE);
    ASSERT_FALSE(has_capability(CAP_DAC_OVERRIDE));
    ASSERT_TRUE(has_capability(CAP_FOWNER));
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), NULL, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  helper.RunInForkedProcess([this] {
    unset_capability(CAP_FOWNER);
    ASSERT_TRUE(has_capability(CAP_DAC_OVERRIDE));
    ASSERT_FALSE(has_capability(CAP_FOWNER));
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), NULL, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  helper.RunInForkedProcess([this] {
    unset_capability(CAP_DAC_OVERRIDE);
    unset_capability(CAP_FOWNER);
    ASSERT_FALSE(has_capability(CAP_DAC_OVERRIDE));
    ASSERT_FALSE(has_capability(CAP_FOWNER));
    EXPECT_NE(0, utimensat(-1, test_file_.c_str(), NULL, 0));
  });
  EXPECT_TRUE(helper.WaitForChildren());

  // Non file owner without write permissions can set the time to some other
  // value with the CAP_FOWNER capability.
  helper.RunInForkedProcess([this] {
    unset_capability(CAP_DAC_OVERRIDE);
    ASSERT_FALSE(has_capability(CAP_DAC_OVERRIDE));
    ASSERT_TRUE(has_capability(CAP_FOWNER));
    struct timespec times[2] = {{0, 0}};
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), times, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  helper.RunInForkedProcess([this] {
    unset_capability(CAP_DAC_OVERRIDE);
    unset_capability(CAP_FOWNER);
    ASSERT_FALSE(has_capability(CAP_DAC_OVERRIDE));
    ASSERT_FALSE(has_capability(CAP_FOWNER));
    struct timespec times[2] = {{0, 0}};
    EXPECT_NE(0, utimensat(-1, test_file_.c_str(), times, 0));
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_F(UtimensatTest, CanSetOmitTimestampsWithoutPermissions) {
  // Non file owner without write permissions and without the CAP_DAC_OVERRIDE or
  // CAP_FOWNER capability can set the timestamps to UTIME_OMIT.
  ASSERT_EQ(chmod(test_file_.c_str(), 0), 0);
  ForkHelper helper;
  helper.RunInForkedProcess([this] {
    unset_capability(CAP_DAC_OVERRIDE);
    unset_capability(CAP_FOWNER);
    ASSERT_FALSE(has_capability(CAP_DAC_OVERRIDE));
    ASSERT_FALSE(has_capability(CAP_FOWNER));
    struct timespec times[2] = {{0, UTIME_OMIT}, {0, UTIME_OMIT}};
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), times, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_F(UtimensatTest, ReturnsEFAULTOnNullPathAndCWDDirFd) {
  ForkHelper helper;
  helper.RunInForkedProcess([] {
    struct timespec times[2] = {{0, 0}};
    EXPECT_NE(0, syscall(SYS_utimensat, AT_FDCWD, NULL, times, 0));
    EXPECT_EQ(errno, EFAULT);
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_F(UtimensatTest, ReturnsENOENTOnEmptyPath) {
  ForkHelper helper;
  helper.RunInForkedProcess([] {
    EXPECT_NE(0, utimensat(-1, "", NULL, 0));
    EXPECT_EQ(errno, ENOENT);
  });
  EXPECT_TRUE(helper.WaitForChildren());
}
}  // namespace
