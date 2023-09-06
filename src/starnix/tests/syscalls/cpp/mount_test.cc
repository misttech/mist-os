// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <ftw.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <unistd.h>

#include <fstream>
#include <iostream>

#include <gtest/gtest.h>
#include <linux/loop.h>

#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/split_string.h"
#include "src/starnix/tests/syscalls/cpp/proc_test_base.h"
#include "src/starnix/tests/syscalls/cpp/syscall_matchers.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

using ::testing::IsSupersetOf;
using ::testing::UnorderedElementsAreArray;

// Reads and splits the "/proc/self/mountinfo" file.
void ReadMountInfo(std::vector<std::vector<std::string>> *out_data) {
  std::string mountinfo;
  EXPECT_TRUE(files::ReadFileToString("/proc/self/mountinfo", &mountinfo));
  auto lines = SplitString(mountinfo, "\n", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
  for (auto &line : lines) {
    auto parts = SplitStringCopy(line, " ", fxl::kTrimWhitespace, fxl::kSplitWantAll);
    ASSERT_GE(parts.size(), 10U) << line;
    out_data->push_back(parts);
  }
}

// Reads "/proc/self/mountinfo" and returns the line for mount at `path`.
void ReadMountInfoLine(const std::string &path, std::vector<std::string> *out_line) {
  std::vector<std::vector<std::string>> data;
  ASSERT_NO_FATAL_FAILURE(ReadMountInfo(&data));
  for (auto &line : data) {
    if (line[4] == path) {
      *out_line = line;
      return;
    }
  }
  out_line->clear();
}

static bool skip_mount_tests = false;

class MountTest : public ::testing::Test {
 public:
  static void SetUpTestSuite() {
    int rv = unshare(CLONE_NEWNS);
    if (rv == -1 && errno == EPERM) {
      // GTest does not support GTEST_SKIP() from a suite setup, so record that we want to skip
      // every test here and skip in SetUp().
      skip_mount_tests = true;
      return;
    }
    ASSERT_EQ(rv, 0) << "unshare(CLONE_NEWNS) failed: " << strerror(errno) << "(" << errno << ")";
  }

  void SetUp() override {
    if (skip_mount_tests) {
      GTEST_SKIP() << "Permission denied for unshare(CLONE_NEWNS), skipping suite.";
    }

    ASSERT_FALSE(temp_dir_.path().empty());
    ASSERT_THAT(mount(nullptr, temp_dir_.path().c_str(), "tmpfs", 0, nullptr), SyscallSucceeds());

    MakeOwnMount("1");
    MakeDir("1/1");
    MakeOwnMount("2");
    MakeDir("2/2");

    ASSERT_TRUE(FileExists("1/1"));
    ASSERT_TRUE(FileExists("2/2"));
  }

  /// All paths used in test functions are relative to the temp directory. This function makes the
  /// path absolute.
  std::string TestPath(const char *path) const { return temp_dir_.path() + "/" + path; }

  // Create a directory.
  int MakeDir(const char *name) const {
    auto path = TestPath(name);
    return mkdir(path.c_str(), 0777);
  }

  /// Make the directory into a bind mount of itself.
  int MakeOwnMount(const char *name) const {
    int err = MakeDir(name);
    if (err < 0)
      return err;
    return Mount(name, name, MS_BIND);
  }

  // Call mount with a null fstype and data.
  int Mount(const char *src, const char *target, int flags) const {
    return mount(src == nullptr ? nullptr : TestPath(src).c_str(), TestPath(target).c_str(),
                 nullptr, flags, nullptr);
  }

  ::testing::AssertionResult FileExists(const char *name) const {
    auto path = TestPath(name);
    if (access(path.c_str(), F_OK) != 0)
      return ::testing::AssertionFailure() << path << ": " << strerror(errno);
    return ::testing::AssertionSuccess();
  }

 private:
  test_helper::ScopedTempDir temp_dir_;
};

[[maybe_unused]] void DumpMountinfo() {
  int fd = open("/proc/self/mountinfo", O_RDONLY);
  char buf[10000];
  size_t n;
  while ((n = read(fd, buf, sizeof(buf))) > 0)
    write(STDOUT_FILENO, buf, n);
  close(fd);
}

#define ASSERT_SUCCESS(call) ASSERT_THAT((call), SyscallSucceeds())

TEST_F(MountTest, RecursiveBind) {
  // Make some mounts
  ASSERT_SUCCESS(MakeDir("a"));
  ASSERT_SUCCESS(Mount("1", "a", MS_BIND));
  ASSERT_SUCCESS(Mount("2", "a/1", MS_BIND));
  ASSERT_TRUE(FileExists("a/1"));
  ASSERT_TRUE(FileExists("a/1/2"));

  // Copy the tree
  ASSERT_SUCCESS(MakeDir("b"));
  ASSERT_SUCCESS(Mount("a", "b", MS_BIND | MS_REC));
  ASSERT_TRUE(FileExists("b/1"));
  ASSERT_TRUE(FileExists("b/1/2"));
}

TEST_F(MountTest, BindIgnoresSharingFlags) {
  ASSERT_SUCCESS(MakeDir("a"));
  // The bind mount should ignore the MS_SHARED flag, so we should end up with non-shared mounts.
  ASSERT_SUCCESS(Mount("1", "a", MS_BIND | MS_SHARED));
  ASSERT_SUCCESS(MakeDir("b"));
  ASSERT_SUCCESS(Mount("a", "b", MS_BIND | MS_SHARED));

  ASSERT_SUCCESS(Mount("2", "a/1", MS_BIND));
  ASSERT_TRUE(FileExists("a/1/2"));
  ASSERT_FALSE(FileExists("b/1/2"));
}

TEST_F(MountTest, BasicSharing) {
  ASSERT_SUCCESS(MakeDir("a"));
  ASSERT_SUCCESS(Mount("1", "a", MS_BIND));
  // Must be done in two steps! MS_BIND | MS_SHARED just ignores the MS_SHARED
  ASSERT_SUCCESS(Mount(nullptr, "a", MS_SHARED));
  ASSERT_SUCCESS(MakeDir("b"));
  ASSERT_SUCCESS(Mount("a", "b", MS_BIND));

  ASSERT_SUCCESS(Mount("2", "a/1", MS_BIND));
  ASSERT_TRUE(FileExists("a/1/2"));
  ASSERT_TRUE(FileExists("b/1/2"));
  ASSERT_FALSE(FileExists("1/1/2"));
}

TEST_F(MountTest, FlagVerification) {
  ASSERT_THAT(Mount(nullptr, "1", MS_SHARED | MS_PRIVATE), SyscallFailsWithErrno(EINVAL));
  ASSERT_THAT(Mount(nullptr, "1", MS_SHARED | MS_NOUSER), SyscallFailsWithErrno(EINVAL));
  ASSERT_THAT(Mount(nullptr, "1", MS_SHARED | MS_SILENT), SyscallSucceeds());
}

// Quiz question B from https://www.kernel.org/doc/Documentation/filesystems/sharedsubtree.txt
TEST_F(MountTest, QuizBRecursion) {
  // Create a hierarchy
  ASSERT_SUCCESS(MakeDir("a"));
  ASSERT_SUCCESS(Mount("1", "a", MS_BIND));
  ASSERT_SUCCESS(Mount("2", "a/1", MS_BIND));

  // Make it shared
  ASSERT_SUCCESS(Mount(nullptr, "a", MS_SHARED | MS_REC));

  // Clone it into itself
  ASSERT_SUCCESS(Mount("a", "a/1/2", MS_BIND | MS_REC));
  ASSERT_TRUE(FileExists("a/1/2/1/2"));
  ASSERT_FALSE(FileExists("a/1/2/1/2/1/2"));
  ASSERT_FALSE(FileExists("a/1/2/1/2/1/2/1/2"));
}

// Quiz question C from https://www.kernel.org/doc/Documentation/filesystems/sharedsubtree.txt
TEST_F(MountTest, QuizCPropagation) {
  ASSERT_SUCCESS(Mount(nullptr, "1", MS_SHARED));
  ASSERT_SUCCESS(MakeDir("1/1/2"));
  ASSERT_SUCCESS(MakeDir("1/1/2/3"));
  ASSERT_SUCCESS(MakeDir("1/1/test"));

  ASSERT_SUCCESS(MakeDir("a"));
  ASSERT_SUCCESS(Mount("1/1", "a", MS_BIND));
  ASSERT_SUCCESS(Mount(nullptr, "1", MS_SLAVE));
  ASSERT_SUCCESS(Mount(nullptr, "1", MS_SHARED));
  ASSERT_SUCCESS(MakeDir("b"));
  ASSERT_SUCCESS(Mount("1/1/2", "b", MS_BIND));
  ASSERT_SUCCESS(Mount(nullptr, "1", MS_SLAVE));

  ASSERT_SUCCESS(Mount("2", "a/test", MS_BIND));
  ASSERT_TRUE(FileExists("1/1/test/2"));
}

TEST_F(MountTest, PropagateOntoMountRoot) {
  ASSERT_SUCCESS(Mount(nullptr, "1", MS_SHARED));
  ASSERT_SUCCESS(MakeDir("1/1/1"));
  ASSERT_SUCCESS(MakeDir("a"));
  ASSERT_SUCCESS(Mount("1/1", "a", MS_BIND));
  // The propagation of this should be equivalent to shadowing the "a" mount.
  ASSERT_SUCCESS(Mount("2", "1/1", MS_BIND));
  ASSERT_TRUE(FileExists("a/2"));
}

TEST_F(MountTest, InheritShared) {
  ASSERT_SUCCESS(MakeDir("a"));
  ASSERT_SUCCESS(Mount("1", "a", MS_BIND));
  ASSERT_SUCCESS(Mount(nullptr, "a", MS_SHARED));
  ASSERT_SUCCESS(Mount("2", "a/1", MS_BIND));
  ASSERT_SUCCESS(MakeDir("b"));
  ASSERT_SUCCESS(Mount("a/1", "b", MS_BIND));
  ASSERT_SUCCESS(Mount("1", "b/2", MS_BIND));
  ASSERT_TRUE(FileExists("a/1/2/1"));
}

TEST_F(MountTest, LotsOfShadowing) {
  ASSERT_SUCCESS(MakeDir("a"));
  ASSERT_SUCCESS(Mount(nullptr, "1", MS_SHARED));
  ASSERT_SUCCESS(Mount("1", "a", MS_BIND));
  ASSERT_SUCCESS(Mount("1", "a", MS_BIND));
  ASSERT_SUCCESS(Mount("1", "a", MS_BIND));
}

// Check that correct mount root is reported in in `/proc/<pid>/mountinfo`.
TEST_F(MountTest, ProcMountInfoRoot) {
  ASSERT_SUCCESS(MakeDir("a"));
  ASSERT_SUCCESS(MakeDir("a/foo"));
  ASSERT_SUCCESS(MakeDir("b"));
  ASSERT_SUCCESS(Mount("a/foo", "b", MS_BIND));

  std::vector<std::string> line;
  ASSERT_NO_FATAL_FAILURE(ReadMountInfoLine(TestPath("b"), &line));
  ASSERT_FALSE(line.empty());
  EXPECT_EQ(line[3], "/a/foo");

  ASSERT_THAT(rmdir(TestPath("a/foo").c_str()), SyscallSucceeds());

  ASSERT_NO_FATAL_FAILURE(ReadMountInfoLine(TestPath("b"), &line));
  ASSERT_FALSE(line.empty());
  EXPECT_EQ(line[3], "/a/foo//deleted");
}

TEST_F(MountTest, Ext4ReadOnlySmokeTest) {
  std::string expected_contents;
  EXPECT_TRUE(files::ReadFileToString("data/hello_world.txt", &expected_contents));

  // TODO(https://fxbug.dev/130532) remove explicit loopback discovery and binding once unneeded
  test_helper::ScopedFD loop_control(open("/dev/loop-control", O_RDWR, 0777));
  ASSERT_TRUE(loop_control.is_valid());

  int free_loop_device_num(ioctl(loop_control.get(), LOOP_CTL_GET_FREE, nullptr));
  ASSERT_TRUE(free_loop_device_num >= 0);

  std::string loop_device_path = "/dev/loop" + std::to_string(free_loop_device_num);
  test_helper::ScopedFD free_loop_device(open(loop_device_path.c_str(), O_RDONLY, 0644));
  ASSERT_TRUE(free_loop_device.is_valid());

  test_helper::ScopedFD ext_image(open("data/simple_ext4.img", O_RDONLY, 0644));
  ASSERT_TRUE(ext_image.is_valid());

  ASSERT_SUCCESS(ioctl(free_loop_device.get(), LOOP_SET_FD, ext_image.get()));

  ASSERT_SUCCESS(MakeDir("basic_ext4"));
  ASSERT_SUCCESS(
      mount(loop_device_path.c_str(), TestPath("basic_ext4").c_str(), "ext4", MS_RDONLY, nullptr));

  std::string observed_contents;
  EXPECT_TRUE(files::ReadFileToString(TestPath("basic_ext4/hello_world.txt"), &observed_contents));

  ASSERT_EQ(expected_contents, observed_contents);
}

// TODO(tbodt): write more tests:
// - A and B are shared, make B downstream, make A private, should now both be private

class ProcMountsTest : public ProcTestBase {
  // Note that these tests can be affected by those in other suites e.g. a
  // MountTest above that doesn't clean up its mounts may change the value of
  // /proc/mounts observed by these tests. Ideally, we'd run a each suite in a
  // different process (and mount namespace, as is already the case) to minimise
  // the blast radius.

 public:
  std::vector<std::string> read_mounts() {
    std::vector<std::string> ret;

    std::ifstream ifs(proc_path() + "/mounts");
    std::string s;
    while (getline(ifs, s)) {
      ret.push_back(s);
    }
    return ret;
  }
};

TEST_F(ProcMountsTest, Basic) {
  // This test assumes the mounts are very specific, so is too brittle to run on Linux.
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "ProcMountsTest::Basic can not be run on Linux, skipping.";
  }
  EXPECT_THAT(read_mounts(), IsSupersetOf({
                                 "data/system / remote_bundle rw 0 0",
                                 "none /dev tmpfs rw 0 0",
                                 ". /tmp tmpfs rw 0 0",
                             }));
}

TEST_F(ProcMountsTest, MountAdded) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping.";
  }

  auto before_mounts = read_mounts();

  std::optional<test_helper::ScopedTempDir> temp_dir;
  temp_dir.emplace();

  ASSERT_THAT(chmod(temp_dir->path().c_str(), 0777), SyscallSucceeds());
  ASSERT_THAT(mount("testtmp", temp_dir->path().c_str(), "tmpfs", 0, nullptr), SyscallSucceeds());

  auto expected_mounts = before_mounts;
  std::string mount = "testtmp " + temp_dir->path() + " tmpfs rw 0 0";
  expected_mounts.push_back(mount);
  EXPECT_THAT(read_mounts(), UnorderedElementsAreArray(expected_mounts));

  // Clean-up.
  temp_dir.reset();

  EXPECT_THAT(read_mounts(), UnorderedElementsAreArray(before_mounts));
}

}  // namespace
