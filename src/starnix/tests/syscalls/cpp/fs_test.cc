// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <lib/fit/defer.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>

#include <algorithm>
#include <climits>
#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include <gtest/gtest.h>
#include <linux/capability.h>

#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

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

// This test passes non-null arguments and has other quirks that fail under sanitizers.
#if (!__has_feature(address_sanitizer) && !defined(__arm__))
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
#endif

constexpr uid_t kOwnerUid = 65534;
constexpr uid_t kNonOwnerUid = 65533;
constexpr gid_t kOwnerGid = 65534;
constexpr gid_t kNonOwnerGid = 65533;

constexpr uid_t kUser1Uid = 65532;
constexpr uid_t kUser2Uid = 65531;
constexpr gid_t kUser1Gid = 65532;
constexpr gid_t kUser2Gid = 65531;

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

bool change_ids(uid_t user, gid_t group) {
  // TODO(https://fxbug.dev/42076425): changing the filesystem user ID from 0 to
  // nonzero should drop capabilities, dropping them manually as a workaround.
  uid_t current_ruid, current_euid, current_suid;
  SAFE_SYSCALL(getresuid(&current_ruid, &current_euid, &current_suid));
  if (current_euid == 0 && user != 0) {
    test_helper::UnsetCapability(CAP_DAC_OVERRIDE);
    test_helper::UnsetCapability(CAP_FOWNER);
  }

  return (setresgid(group, group, group) == 0) && (setresuid(user, user, user) == 0);
}

TEST_F(UtimensatTest, OwnerCanAlwaysSetTime) {
  ASSERT_EQ(chmod(test_file_.c_str(), 0), 0);

  // File owner can change time to now even without write perms.
  test_helper::ForkHelper helper;
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
  test_helper::ForkHelper helper;
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
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([this] {
    ASSERT_TRUE(test_helper::HasCapability(CAP_DAC_OVERRIDE));
    ASSERT_TRUE(test_helper::HasCapability(CAP_FOWNER));
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), NULL, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  helper.RunInForkedProcess([this] {
    test_helper::UnsetCapability(CAP_DAC_OVERRIDE);
    ASSERT_FALSE(test_helper::HasCapability(CAP_DAC_OVERRIDE));
    ASSERT_TRUE(test_helper::HasCapability(CAP_FOWNER));
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), NULL, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  helper.RunInForkedProcess([this] {
    test_helper::UnsetCapability(CAP_FOWNER);
    ASSERT_TRUE(test_helper::HasCapability(CAP_DAC_OVERRIDE));
    ASSERT_FALSE(test_helper::HasCapability(CAP_FOWNER));
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), NULL, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  helper.RunInForkedProcess([this] {
    test_helper::UnsetCapability(CAP_DAC_OVERRIDE);
    test_helper::UnsetCapability(CAP_FOWNER);
    ASSERT_FALSE(test_helper::HasCapability(CAP_DAC_OVERRIDE));
    ASSERT_FALSE(test_helper::HasCapability(CAP_FOWNER));
    EXPECT_NE(0, utimensat(-1, test_file_.c_str(), NULL, 0));
  });
  EXPECT_TRUE(helper.WaitForChildren());

  // Non file owner without write permissions can set the time to some other
  // value with the CAP_FOWNER capability.
  helper.RunInForkedProcess([this] {
    test_helper::UnsetCapability(CAP_DAC_OVERRIDE);
    ASSERT_FALSE(test_helper::HasCapability(CAP_DAC_OVERRIDE));
    ASSERT_TRUE(test_helper::HasCapability(CAP_FOWNER));
    struct timespec times[2] = {{0, 0}};
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), times, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());

  helper.RunInForkedProcess([this] {
    test_helper::UnsetCapability(CAP_DAC_OVERRIDE);
    test_helper::UnsetCapability(CAP_FOWNER);
    ASSERT_FALSE(test_helper::HasCapability(CAP_DAC_OVERRIDE));
    ASSERT_FALSE(test_helper::HasCapability(CAP_FOWNER));
    struct timespec times[2] = {{0, 0}};
    EXPECT_NE(0, utimensat(-1, test_file_.c_str(), times, 0));
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_F(UtimensatTest, CanSetOmitTimestampsWithoutPermissions) {
  // Non file owner without write permissions and without the CAP_DAC_OVERRIDE or
  // CAP_FOWNER capability can set the timestamps to UTIME_OMIT.
  ASSERT_EQ(chmod(test_file_.c_str(), 0), 0);
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([this] {
    test_helper::UnsetCapability(CAP_DAC_OVERRIDE);
    test_helper::UnsetCapability(CAP_FOWNER);
    ASSERT_FALSE(test_helper::HasCapability(CAP_DAC_OVERRIDE));
    ASSERT_FALSE(test_helper::HasCapability(CAP_FOWNER));
    struct timespec times[2] = {{0, UTIME_OMIT}, {0, UTIME_OMIT}};
    EXPECT_EQ(0, utimensat(-1, test_file_.c_str(), times, 0))
        << "utimensat failed: " << std::strerror(errno);
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_F(UtimensatTest, ReturnsEFAULTOnNullPathAndCWDDirFd) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    struct timespec times[2] = {{0, 0}};
    EXPECT_NE(0, syscall(SYS_utimensat, AT_FDCWD, NULL, times, 0));
    EXPECT_EQ(errno, EFAULT);
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_F(UtimensatTest, ReturnsENOENTOnEmptyPath) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    EXPECT_NE(0, utimensat(-1, "", NULL, 0));
    EXPECT_EQ(errno, ENOENT);
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

std::optional<std::string> MountOverlayFs(const std::string &temp_dir) {
  EXPECT_FALSE(temp_dir.empty());

  std::string overlay = temp_dir + "/overlay";
  EXPECT_THAT(mkdir(overlay.c_str(), S_IRWXU), SyscallSucceeds());

  std::string lower = temp_dir + "/lower";
  EXPECT_THAT(mkdir(lower.c_str(), S_IRWXU), SyscallSucceeds());

  std::string upper = temp_dir + "/upper";
  EXPECT_THAT(mkdir(upper.c_str(), S_IRWXU), SyscallSucceeds());

  std::string work = temp_dir + "/work";
  EXPECT_THAT(mkdir(work.c_str(), S_IRWXU), SyscallSucceeds());

  std::string options = fxl::StringPrintf("lowerdir=%s,upperdir=%s,workdir=%s", lower.c_str(),
                                          upper.c_str(), work.c_str());

  int res = mount(nullptr, overlay.c_str(), "overlay", 0, options.c_str());
  EXPECT_EQ(res, 0) << "mount: " << std::strerror(errno);

  if (res != 0) {
    return std::nullopt;
  }

  return overlay;
}

std::optional<std::string> MountTmpFs(const std::string &temp_dir) {
  std::string temp = temp_dir + "/tmp";
  EXPECT_THAT(mkdir(temp.c_str(), S_IRWXU), SyscallSucceeds());

  int res = mount(nullptr, temp.c_str(), "tmpfs", 0, "");
  EXPECT_EQ(res, 0) << "mount: " << std::strerror(errno);

  if (res != 0) {
    return std::nullopt;
  }

  return temp;
}

class FsMountTest
    : public testing::TestWithParam<std::optional<std::string> (*)(const std::string &)> {
 protected:
  void SetUp() override {
    // TODO(https://fxbug.dev/317285180) don't skip on baseline
    if (!test_helper::HasSysAdmin()) {
      GTEST_SKIP() << "Not running with sysadmin capabilities, skipping suite.";
    }
    auto mounter = GetParam();
    auto mounted = mounter(temp_dir_.path());
    ASSERT_TRUE(mounted.has_value()) << "failed to mount fs";
    mount_path_ = mounted.value();

    // Directory Permissions: owner can do everything, user and other can search.
    constexpr int kDirPerms = S_IRWXU | S_IXGRP | S_IXOTH;

    ASSERT_THAT(chmod(mount_path_.c_str(), kDirPerms), SyscallSucceeds());
    ASSERT_THAT(chmod(temp_dir_.path().c_str(), kDirPerms), SyscallSucceeds());
  }

  test_helper::ScopedTempDir temp_dir_;
  std::string mount_path_;
};

INSTANTIATE_TEST_SUITE_P(TmpFs, FsMountTest, ::testing::Values(MountTmpFs));
INSTANTIATE_TEST_SUITE_P(OverlayFs, FsMountTest, ::testing::Values(MountOverlayFs));

TEST_P(FsMountTest, CantBypassDirectoryPermissions) {
  std::string user1_folder = mount_path_ + "/user1";
  ASSERT_THAT(mkdir(user1_folder.c_str(), S_IRWXU), SyscallSucceeds());
  ASSERT_THAT(chown(user1_folder.c_str(), kUser1Uid, kUser1Gid), SyscallSucceeds());

  std::string user2_folder = mount_path_ + "/user2";
  ASSERT_THAT(mkdir(user2_folder.c_str(), S_IRWXU), SyscallSucceeds());
  ASSERT_THAT(chown(user2_folder.c_str(), kUser2Uid, kUser2Gid), SyscallSucceeds());

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    ASSERT_TRUE(change_ids(kUser2Uid, kUser2Gid));
    test_helper::DropAllCapabilities();

    // We should be able to create files in user2's directory.
    std::string file_path = user2_folder + "/test_file";
    int fd = open(file_path.c_str(), O_RDWR | O_CREAT | O_EXCL, S_IRUSR | S_IWUSR);
    EXPECT_NE(fd, -1) << "open " << file_path << ": " << std::strerror(errno);
    if (fd != -1) {
      close(fd);
      EXPECT_EQ(unlink(file_path.c_str()), 0);
    }

    // We shouldn't be able to create files in user1's directory.
    file_path = user1_folder + "/test_file";
    fd = open(file_path.c_str(), O_RDWR | O_CREAT | O_EXCL, S_IRUSR | S_IWUSR);
    EXPECT_EQ(fd, -1);
    EXPECT_EQ(errno, EACCES);
    if (fd != -1) {
      close(fd);
      EXPECT_EQ(unlink(file_path.c_str()), 0);
    }
  });
}

TEST_P(FsMountTest, CreateWithDifferentModes) {
  std::string user1_folder = mount_path_ + "/user1";
  ASSERT_THAT(mkdir(user1_folder.c_str(), S_IRWXU), SyscallSucceeds());
  ASSERT_THAT(chown(user1_folder.c_str(), kUser1Uid, kUser1Gid), SyscallSucceeds());

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([user1_folder] {
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));
    test_helper::DropAllCapabilities();

    const mode_t old_umask = umask(0);
    constexpr mode_t kModeMask = 0777;
    auto clean_umask = fit::defer([old_umask]() { umask(old_umask); });

    for (mode_t mode = 0000; mode <= 0777; mode++) {
      SCOPED_TRACE(fxl::StringPrintf("Mode: %o", mode));

      std::string file_path = fxl::StringPrintf("%s/create.%o", user1_folder.c_str(), mode);
      {
        fbl::unique_fd fd(open(file_path.c_str(), O_RDWR | O_CREAT | O_EXCL, mode));
        EXPECT_TRUE(fd.is_valid()) << "open: " << std::strerror(errno);
      }

      auto cleanup =
          fit::defer([file_path]() { EXPECT_THAT(unlink(file_path.c_str()), SyscallSucceeds()); });

      struct stat file_stat;
      EXPECT_THAT(stat(file_path.c_str(), &file_stat), SyscallSucceeds());
      EXPECT_TRUE(file_stat.st_mode & S_IFREG) << "not a regular file";
      EXPECT_EQ(file_stat.st_mode & kModeMask, mode) << "wrong permissions";
    }
  });
}

TEST_P(FsMountTest, ChmodWithDifferentModes) {
  std::string user1_folder = mount_path_ + "/user1";
  ASSERT_THAT(mkdir(user1_folder.c_str(), S_IRWXU), SyscallSucceeds());
  ASSERT_THAT(chown(user1_folder.c_str(), kUser1Uid, kUser1Gid), SyscallSucceeds());

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([user1_folder] {
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));
    test_helper::DropAllCapabilities();
    const mode_t old_umask = umask(0);
    constexpr mode_t kModeMask = 0777;
    auto clean_umask = fit::defer([old_umask]() { umask(old_umask); });

    for (mode_t mode = 0000; mode <= 0777; mode++) {
      SCOPED_TRACE(fxl::StringPrintf("Mode: %o", mode));

      std::string file_path = fxl::StringPrintf("%s/chmod.%o", user1_folder.c_str(), mode);
      {
        fbl::unique_fd fd(open(file_path.c_str(), O_RDWR | O_CREAT | O_EXCL, S_IRUSR | S_IWUSR));
        EXPECT_TRUE(fd.is_valid()) << "open: " << std::strerror(errno);
      }
      auto cleanup =
          fit::defer([file_path]() { EXPECT_THAT(unlink(file_path.c_str()), SyscallSucceeds()); });

      EXPECT_THAT(chmod(file_path.c_str(), mode), SyscallSucceeds());

      struct stat file_stat;
      EXPECT_THAT(stat(file_path.c_str(), &file_stat), SyscallSucceeds());
      EXPECT_TRUE(file_stat.st_mode & S_IFREG) << "not a regular file";
      EXPECT_EQ(file_stat.st_mode & kModeMask, mode) << "wrong permissions";
    }
  });
}

TEST_P(FsMountTest, ChownMinusOneSucceeds) {
  // Executing chown(file, -1, -1) should almost always work.
  std::string user1_file = files::JoinPath(mount_path_, "user1_file");
  close(SAFE_SYSCALL(creat(user1_file.c_str(), S_IRWXU)));
  SAFE_SYSCALL(chown(user1_file.c_str(), kUser1Uid, kUser1Gid));

  test_helper::ForkHelper helper;

  // Running as the same user.
  helper.RunInForkedProcess([user1_file] {
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));
    test_helper::DropAllCapabilities();

    EXPECT_THAT(chown(user1_file.c_str(), -1, -1), SyscallSucceeds());
  });
  EXPECT_TRUE(helper.WaitForChildren());

  // Running as a different user.
  helper.RunInForkedProcess([user1_file] {
    ASSERT_TRUE(change_ids(kUser2Uid, kUser2Gid));
    test_helper::DropAllCapabilities();

    EXPECT_THAT(chown(user1_file.c_str(), -1, -1), SyscallSucceeds());
  });
  EXPECT_TRUE(helper.WaitForChildren());

  SAFE_SYSCALL(unlink(user1_file.c_str()));
}

TEST_P(FsMountTest, ChownMinusOneNoPathAccessFails) {
  // Executing chown(file, -1, -1) fails if we can't resolve the path.
  std::string user1_folder = files::JoinPath(mount_path_, "user1_folder");
  std::string user1_file = files::JoinPath(user1_folder, "user1_file");
  SAFE_SYSCALL(mkdir(user1_folder.c_str(), S_IRWXU));  // user2 can't access.

  SAFE_SYSCALL(chown(user1_folder.c_str(), kUser1Uid, kUser1Gid));
  close(SAFE_SYSCALL(creat(user1_file.c_str(), S_IRWXU)));
  SAFE_SYSCALL(chown(user1_file.c_str(), kUser1Uid, kUser1Gid));

  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([user1_folder, user1_file] {
    ASSERT_TRUE(change_ids(kUser2Uid, kUser2Gid));
    test_helper::DropAllCapabilities();

    EXPECT_THAT(chown(user1_folder.c_str(), -1, -1), SyscallSucceeds());
    EXPECT_THAT(chown(user1_file.c_str(), -1, -1), SyscallFailsWithErrno(EACCES));
  });
  EXPECT_TRUE(helper.WaitForChildren());

  SAFE_SYSCALL(unlink(user1_file.c_str()));
}

TEST_P(FsMountTest, ChownMinusOneOnSIDFileFails) {
  // Executing chown(file, -1, -1) fails if the file is set-ID.
  std::string user1_file = files::JoinPath(mount_path_, "user1_file");
  close(SAFE_SYSCALL(creat(user1_file.c_str(), 0)));
  SAFE_SYSCALL(chown(user1_file.c_str(), kUser1Uid, kUser1Gid));

  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([user1_file] {
    SAFE_SYSCALL(chmod(user1_file.c_str(), S_ISUID));
    ASSERT_TRUE(change_ids(kUser2Uid, kUser2Gid));
    test_helper::DropAllCapabilities();

    EXPECT_THAT(chown(user1_file.c_str(), -1, -1), SyscallFailsWithErrno(EPERM));
  });
  EXPECT_TRUE(helper.WaitForChildren());

  // The file should still be set-user-ID even after failure.
  struct stat file_stat{};
  SAFE_SYSCALL(stat(user1_file.c_str(), &file_stat));
  EXPECT_NE(file_stat.st_mode & S_ISUID, 0U);

  helper.RunInForkedProcess([user1_file] {
    SAFE_SYSCALL(chmod(user1_file.c_str(), S_ISGID | S_IXGRP));
    ASSERT_TRUE(change_ids(kUser2Uid, kUser2Gid));
    test_helper::DropAllCapabilities();

    EXPECT_THAT(chown(user1_file.c_str(), -1, -1), SyscallFailsWithErrno(EPERM));
  });
  EXPECT_TRUE(helper.WaitForChildren());

  // The file should still be set-group-ID even after failure.
  SAFE_SYSCALL(stat(user1_file.c_str(), &file_stat));
  EXPECT_EQ(file_stat.st_mode & (S_ISGID | S_IXGRP), (unsigned int)(S_ISGID | S_IXGRP));

  // But not if we are the owners.
  helper.RunInForkedProcess([user1_file] {
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));
    test_helper::DropAllCapabilities();

    EXPECT_THAT(chown(user1_file.c_str(), -1, -1), SyscallSucceeds());
  });
  EXPECT_TRUE(helper.WaitForChildren());

  // Doing a successful chown should have dropped the set-user-ID bit of the file.
  SAFE_SYSCALL(stat(user1_file.c_str(), &file_stat));
  EXPECT_EQ(file_stat.st_mode & S_ISUID, 0U);

  SAFE_SYSCALL(unlink(user1_file.c_str()));
}

TEST_P(FsMountTest, ChownSameOwnerAndGroupFails) {
  // Executing chown explicitly specifying owner and gid (instead of -1), fails
  // if we are not owners.
  std::string user1_file = files::JoinPath(mount_path_, "user1_file");
  close(SAFE_SYSCALL(creat(user1_file.c_str(), S_IRWXU)));
  SAFE_SYSCALL(chmod(user1_file.c_str(), S_IRWXU));
  SAFE_SYSCALL(chown(user1_file.c_str(), kUser1Uid, kUser1Gid));

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([user1_file] {
    ASSERT_TRUE(change_ids(kUser2Uid, kUser2Gid));
    test_helper::DropAllCapabilities();

    EXPECT_THAT(chown(user1_file.c_str(), kUser1Uid, kUser1Gid), SyscallFailsWithErrno(EPERM));
    EXPECT_THAT(chown(user1_file.c_str(), -1, kUser1Gid), SyscallFailsWithErrno(EPERM));
    EXPECT_THAT(chown(user1_file.c_str(), kUser1Uid, -1), SyscallFailsWithErrno(EPERM));
  });
  EXPECT_TRUE(helper.WaitForChildren());

  SAFE_SYSCALL(unlink(user1_file.c_str()));
}

TEST_P(FsMountTest, ChownOnSUIDFileDropsSUIDBit) {
  std::string user1_file = files::JoinPath(mount_path_, "user1_file");
  close(SAFE_SYSCALL(creat(user1_file.c_str(), 0)));
  SAFE_SYSCALL(chown(user1_file.c_str(), kUser1Uid, kUser1Gid));

  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([user1_file] {
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));
    test_helper::DropAllCapabilities();

    for (mode_t mode = 0000; mode <= 0777; mode++) {
      SCOPED_TRACE(fxl::StringPrintf("Mode: %o", mode));
      SAFE_SYSCALL(chmod(user1_file.c_str(), S_ISUID | mode));
      SAFE_SYSCALL(chown(user1_file.c_str(), -1, -1));

      struct stat file_stat{};
      SAFE_SYSCALL(stat(user1_file.c_str(), &file_stat));
      EXPECT_EQ(file_stat.st_mode & S_ISUID, 0U);
    }
  });

  EXPECT_TRUE(helper.WaitForChildren());
}
TEST_P(FsMountTest, ChownOnSGIDFileDropsSGIDBit) {
  std::string user1_file = files::JoinPath(mount_path_, "user1_file");
  close(SAFE_SYSCALL(creat(user1_file.c_str(), 0)));
  SAFE_SYSCALL(chown(user1_file.c_str(), kUser1Uid, kUser1Gid));

  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([user1_file] {
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));
    test_helper::DropAllCapabilities();

    for (mode_t mode = 0000; mode <= 0777; mode++) {
      SCOPED_TRACE(fxl::StringPrintf("Mode: %o", mode));
      SAFE_SYSCALL(chmod(user1_file.c_str(), S_ISGID | mode));
      SAFE_SYSCALL(chown(user1_file.c_str(), -1, -1));

      struct stat file_stat{};
      SAFE_SYSCALL(stat(user1_file.c_str(), &file_stat));
      if (mode & S_IXGRP) {
        // The set-group-ID bit only takes effect if the file is
        // group-executable. Otherwise it has other meaning and should not drop
        // that bit upon chown.
        EXPECT_EQ(file_stat.st_mode & S_ISGID, 0U);
      } else {
        EXPECT_NE(file_stat.st_mode & S_ISGID, 0U);
      }
    }
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_P(FsMountTest, OpenWithTruncAndCreatOnReadOnlyFsReturnsEROFS) {
  std::string lock_file = mount_path_ + "/lock";
  int fd = SAFE_SYSCALL(open(lock_file.c_str(), O_CREAT | O_RDWR, 0600));
  close(fd);

  SAFE_SYSCALL(chown(lock_file.c_str(), kUser1Uid, kUser1Gid));

  // Remount filesystem as read-only.
  SAFE_SYSCALL(
      mount(nullptr, mount_path_.c_str(), "ignored", MS_REMOUNT | MS_BIND | MS_RDONLY, ""));
  auto cleanup = fit::defer([this]() {
    SAFE_SYSCALL(mount(nullptr, mount_path_.c_str(), "ignored", MS_REMOUNT | MS_BIND, ""));
  });

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([lock_file] {
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));
    test_helper::DropAllCapabilities();

    int fd = open(lock_file.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0666);
    int saved_errno = errno;
    EXPECT_EQ(fd, -1);
    EXPECT_EQ(saved_errno, EROFS) << std::strerror(saved_errno);

    if (fd != -1) {
      SAFE_SYSCALL(close(fd));
    }
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_P(FsMountTest, OpenWithTruncAndCreatWithExistingFileSucceeds) {
  std::string lock_file = mount_path_ + "/lock";
  int fd = SAFE_SYSCALL(open(lock_file.c_str(), O_CREAT | O_RDWR, 0600));
  close(fd);

  SAFE_SYSCALL(chown(lock_file.c_str(), kUser1Uid, kUser1Gid));

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([lock_file] {
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));
    test_helper::DropAllCapabilities();

    int fd = SAFE_SYSCALL(open(lock_file.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0600));
    SAFE_SYSCALL(close(fd));
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_P(FsMountTest, OpenWithTruncAndCreatWithNoPermsReturnsEACCES) {
  std::string lock_file = mount_path_ + "/lock";
  int fd = SAFE_SYSCALL(open(lock_file.c_str(), O_CREAT | O_RDWR, 0600));
  close(fd);

  SAFE_SYSCALL(chown(lock_file.c_str(), kUser1Uid, kUser1Gid));

  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([lock_file] {
    ASSERT_TRUE(change_ids(kUser2Uid, kUser2Gid));
    test_helper::DropAllCapabilities();

    int fd = open(lock_file.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0600);
    int saved_errno = errno;
    EXPECT_EQ(fd, -1);
    EXPECT_EQ(saved_errno, EACCES) << std::strerror(saved_errno);
    if (fd != -1) {
      SAFE_SYSCALL(close(fd));
    }
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST_P(FsMountTest, CreateAndRenameDirectory) {
  std::string old_name = mount_path_ + "/old";
  std::string new_name = mount_path_ + "/new";

  ASSERT_THAT(mkdir(old_name.c_str(), 0700), SyscallSucceeds());
  EXPECT_THAT(rename(old_name.c_str(), new_name.c_str()), SyscallSucceeds());
}

class OtmpfileTest : public ::testing::Test {
 protected:
  void SetUp() override {
    char *dir = getenv("MUTABLE_STORAGE");
    test_folder_ = dir == nullptr ? "/tmp/XXXXXX" : std::string(dir) + "/XXXXXX";
    ASSERT_NE(mkdtemp(test_folder_.data()), nullptr)
        << "failed to create test folder: " << std::strerror(errno);

    test_file1_ = test_folder_ + "/testfile1";
    test_file2_ = test_folder_ + "/testfile2";
  }

  void TearDown() override {
    if (tmpfile_fd_ != -1) {
      tmpfile_fd_.reset();
    }
    // These files may have been created, attempt to remove them in case they were.
    remove(test_file1_.c_str());
    remove(test_file2_.c_str());
    if (test_folder_.length() != 0) {
      ASSERT_EQ(rmdir(test_folder_.c_str()), 0);
    }
  }

  fbl::unique_fd tmpfile_fd_;
  std::string test_folder_;
  std::string test_file1_;
  std::string test_file2_;
};

void CheckLinkCount(int fd, unsigned count) {
  struct stat s;
  ASSERT_EQ(fstat(fd, &s), 0);
  ASSERT_EQ(s.st_nlink, count);
}

TEST_F(OtmpfileTest, TmpFileLinkIntoAfter) {
  // CAP_DAC_READ_SEARCH capability is required to use AT_EMPTY_PATH with linkat
  if (!test_helper::HasCapability(CAP_DAC_READ_SEARCH)) {
    GTEST_SKIP() << "Not running with CAP_DAC_READ_SEARCH capabilities, skipping.";
  }
  tmpfile_fd_ = fbl::unique_fd(open(test_folder_.c_str(), O_RDWR | O_TMPFILE));
  ASSERT_TRUE(tmpfile_fd_.is_valid()) << "open() with O_TMPFILE failed:" << strerror(errno);
  ASSERT_NO_FATAL_FAILURE(CheckLinkCount(tmpfile_fd_.get(), 0));

  // Write to file. The contents are used later to verify that linkat worked.
  ASSERT_EQ(write(tmpfile_fd_.get(), "hello", 5), 5)
      << "Write to tmpfile failed:" << strerror(errno);

  // Test that we can link.
  SAFE_SYSCALL(linkat(tmpfile_fd_.get(), "", AT_FDCWD, test_file1_.c_str(), AT_EMPTY_PATH));
  ASSERT_NO_FATAL_FAILURE(CheckLinkCount(tmpfile_fd_.get(), 1));

  // Test that we can link again.
  SAFE_SYSCALL(linkat(tmpfile_fd_.get(), "", AT_FDCWD, test_file2_.c_str(), AT_EMPTY_PATH));
  ASSERT_NO_FATAL_FAILURE(CheckLinkCount(tmpfile_fd_.get(), 2));

  // Verify contents.
  fbl::unique_fd test_file_fd(open(test_file1_.c_str(), O_RDONLY));
  ASSERT_TRUE(test_file_fd.is_valid()) << "Failed to open file:" << strerror(errno);
  char buffer[10];
  ASSERT_EQ(read(test_file_fd.get(), buffer, 10), 5)
      << "Failed to read from file:" << strerror(errno);
  ASSERT_EQ(strncmp(buffer, "hello", 5), 0)
      << "Contents do not match the contents written to the tmpfile.";

  // If we try to link into a path that is already used, this should fail with EEXIST.
  int result = linkat(tmpfile_fd_.get(), "", AT_FDCWD, test_file1_.c_str(), AT_EMPTY_PATH);
  int saved_errno = errno;
  ASSERT_EQ(result, -1);
  EXPECT_EQ(saved_errno, EEXIST) << "Link to an existing path should fail with EEXIST:"
                                 << std::strerror(saved_errno);
}

TEST_F(OtmpfileTest, TmpFileWithOExclShouldFailLinkInto) {
  // CAP_DAC_READ_SEARCH capability is required to use AT_EMPTY_PATH with linkat
  if (!test_helper::HasCapability(CAP_DAC_READ_SEARCH)) {
    GTEST_SKIP() << "Not running with CAP_DAC_READ_SEARCH capabilities, skipping.";
  }

  tmpfile_fd_ = fbl::unique_fd(open(test_folder_.c_str(), O_RDWR | O_TMPFILE | O_EXCL));
  ASSERT_TRUE(tmpfile_fd_.is_valid()) << "open() with O_TMPFILE failed:" << strerror(errno);

  int result = linkat(tmpfile_fd_.get(), "", AT_FDCWD, test_file1_.c_str(), AT_EMPTY_PATH);
  int saved_errno = errno;
  ASSERT_EQ(result, -1);
  EXPECT_EQ(saved_errno, ENOENT)
      << "linkat() should fail when file was opened with O_TMPFILE | O_EXCL with ENOENT:"
      << std::strerror(saved_errno);
}

TEST_F(OtmpfileTest, TmpFileFailWithRdOnlyAccessMode) {
  tmpfile_fd_ = fbl::unique_fd(open(test_folder_.c_str(), O_RDONLY | O_TMPFILE));
  int saved_errno = errno;
  ASSERT_FALSE(tmpfile_fd_.is_valid());
  EXPECT_EQ(saved_errno, EINVAL)
      << "open() with O_TMPFILE not specified with O_RDWR and O_WRONLY should fail with EINVAL:"
      << std::strerror(saved_errno);
}

TEST_F(OtmpfileTest, TmpFileWithOCreatShouldFail) {
  tmpfile_fd_ = fbl::unique_fd(open(test_folder_.c_str(), O_RDWR | O_CREAT | O_TMPFILE));
  int saved_errno = errno;
  ASSERT_FALSE(tmpfile_fd_.is_valid());
  EXPECT_EQ(saved_errno, EINVAL)
      << "open() with O_TMPFILE and O_CREAT are not compatible. Should fail with EINVAL:"
      << std::strerror(saved_errno);
}

TEST(LinkTest, FileLinkCount) {
  // Create a temporary directory, store its absolute path and chdir to it.
  char *dir = getenv("MUTABLE_STORAGE");
  std::string test_folder =
      dir == nullptr ? "/tmp/linkcount.XXXXXX" : std::string(dir) + "/linkcount.XXXXXX";
  ASSERT_NE(mkdtemp(test_folder.data()), nullptr)
      << "failed to create test folder: " << std::strerror(errno);

  std::string test_file = test_folder + "/foo";
  fbl::unique_fd foo_fd(creat(test_file.c_str(), S_IRWXU));
  ASSERT_TRUE(foo_fd.is_valid()) << "Failed to open file:" << strerror(errno);
  ASSERT_NO_FATAL_FAILURE(CheckLinkCount(foo_fd.get(), 1));

  // Create link to the file. We should see link count increment.
  std::string bar = test_folder + "/bar";
  SAFE_SYSCALL(linkat(AT_FDCWD, test_file.c_str(), AT_FDCWD, bar.c_str(), 0));
  ASSERT_NO_FATAL_FAILURE(CheckLinkCount(foo_fd.get(), 2));

  // Unlink should decrement the link count.
  EXPECT_EQ(unlink(bar.c_str()), 0);
  ASSERT_NO_FATAL_FAILURE(CheckLinkCount(foo_fd.get(), 1));
  EXPECT_EQ(unlink(test_file.c_str()), 0);
  ASSERT_NO_FATAL_FAILURE(CheckLinkCount(foo_fd.get(), 0));

  // Clean up.
  ASSERT_EQ(rmdir(test_folder.c_str()), 0);
}

}  // namespace
