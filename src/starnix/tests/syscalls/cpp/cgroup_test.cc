// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <sys/mount.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <string>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

constexpr char CONTROLLERS_FILE[] = "cgroup.controllers";
constexpr char PROCS_FILE[] = "cgroup.procs";

// Mounts cgroup2 in a temporary directory for each test case, and deletes all cgroups created by
// `CreateCgroup` at the end of each test.
class CgroupTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (!test_helper::HasSysAdmin()) {
      // From https://docs.kernel.org/admin-guide/cgroup-v2.html#interaction-with-other-namespaces
      // mounting cgroup requires CAP_SYS_ADMIN.
      GTEST_SKIP() << "requires CAP_SYS_ADMIN to mount cgroup";
    }
    ASSERT_THAT(mkdir(path().c_str(), 0777), SyscallSucceeds());
    ASSERT_THAT(mount(nullptr, path().c_str(), "cgroup2", 0, nullptr), SyscallSucceeds());
  }

  void TearDown() override {
    if (!test_helper::HasSysAdmin()) {
      // `TearDown` is still called for skipped tests, and the below assertions can fail.
      return;
    }

    // Remove paths created by the test in reverse creation order.
    // cgroup2 filesystem persists on the system after umounting, and lingering subdirectories can
    // cause subsequent tests to fail.
    for (auto path = cgroup_paths_.rbegin(); path != cgroup_paths_.rend(); path++) {
      ASSERT_THAT(rmdir(path->c_str()), SyscallSucceeds());
    }
    ASSERT_THAT(umount(path().c_str()), SyscallSucceeds());
  }

  std::string path() { return temp_dir_.path() + "/cgroup"; }

  static void CheckInterfaceFilesExist(const std::string& path) {
    std::string controllers_path = path + "/" + CONTROLLERS_FILE;
    std::string procs_path = path + "/" + PROCS_FILE;

    struct stat buffer;
    ASSERT_THAT(stat(controllers_path.c_str(), &buffer), SyscallSucceeds());
    ASSERT_THAT(stat(procs_path.c_str(), &buffer), SyscallSucceeds());
  }

  struct ExpectedEntry {
    std::string name;
    unsigned char type;
  };
  static void CheckDirectoryIncludes(const std::string& path,
                                     const std::vector<ExpectedEntry>& expected) {
    DIR* dir = opendir(path.c_str());
    ASSERT_TRUE(dir);

    std::unordered_map<std::string, unsigned char> entry_types;
    while (struct dirent* entry = readdir(dir)) {
      entry_types.emplace(std::string(entry->d_name), entry->d_type);
    }
    closedir(dir);

    for (const ExpectedEntry& entry : expected) {
      auto found = entry_types.find(entry.name);
      ASSERT_NE(found, entry_types.end());
      EXPECT_EQ(found->second, entry.type);
    }
  }

  void CreateCgroup(std::string path) {
    ASSERT_THAT(mkdir(path.c_str(), 0777), SyscallSucceeds());
    cgroup_paths_.push_back(std::move(path));
  }

  void DeleteCgroup(const std::string& path) {
    auto it = std::ranges::find(cgroup_paths_, path);
    ASSERT_NE(it, cgroup_paths_.end());
    ASSERT_THAT(rmdir(path.c_str()), SyscallSucceeds());
    cgroup_paths_.erase(it);
  }

 private:
  // Paths to be removed after a test has completed.
  std::vector<std::string> cgroup_paths_;

  test_helper::ScopedTempDir temp_dir_;
};

TEST_F(CgroupTest, InterfaceFilesForRoot) { CheckInterfaceFilesExist(path()); }

// This test checks that nodes created as part of cgroups have the same inode each time it is
// accessed, which is seen on Linux.
TEST_F(CgroupTest, InodeNumbersAreConsistent) {
  std::string controllers_path = path() + "/" + CONTROLLERS_FILE;
  struct stat buffer1, buffer2;
  ASSERT_THAT(stat(controllers_path.c_str(), &buffer1), SyscallSucceeds());
  ASSERT_THAT(stat(controllers_path.c_str(), &buffer2), SyscallSucceeds());
  EXPECT_EQ(buffer1.st_ino, buffer2.st_ino);
}

TEST_F(CgroupTest, ReadDir) {
  CheckDirectoryIncludes(path(), {
                                     {.name = "cgroup.procs", .type = DT_REG},
                                     {.name = "cgroup.controllers", .type = DT_REG},
                                 });

  std::string child1 = "child1";
  CreateCgroup(path() + "/" + child1);
  CheckDirectoryIncludes(path(), {
                                     {.name = "cgroup.procs", .type = DT_REG},
                                     {.name = "cgroup.controllers", .type = DT_REG},
                                     {.name = child1, .type = DT_DIR},
                                 });

  std::string child2 = "child2";
  CreateCgroup(path() + "/" + child2);
  CheckDirectoryIncludes(path(), {
                                     {.name = "cgroup.procs", .type = DT_REG},
                                     {.name = "cgroup.controllers", .type = DT_REG},
                                     {.name = child1, .type = DT_DIR},
                                     {.name = child2, .type = DT_DIR},
                                 });
}

TEST_F(CgroupTest, CreateSubgroups) {
  std::string child1_path = path() + "/child1";
  CreateCgroup(child1_path);
  CheckInterfaceFilesExist(child1_path);

  std::string child2_path = path() + "/child2";
  CreateCgroup(child2_path);
  CheckInterfaceFilesExist(child2_path);

  std::string grandchild_path = path() + "/child2/grandchild";
  CreateCgroup(grandchild_path);
  CheckInterfaceFilesExist(grandchild_path);
}

TEST_F(CgroupTest, CreateSubgroupAlreadyExists) {
  std::string child_path = path() + "/child";
  CreateCgroup(child_path);
  ASSERT_THAT(mkdir(child_path.c_str(), 0777), SyscallFailsWithErrno(EEXIST));
}

TEST_F(CgroupTest, WriteToInterfaceFileAfterCgroupIsDeleted) {
  std::string child_path = path() + "/child";
  std::string child_procs_path = child_path + "/" + PROCS_FILE;

  CreateCgroup(child_path);

  fbl::unique_fd child_procs_fd(open(child_procs_path.c_str(), O_WRONLY));
  ASSERT_TRUE(child_procs_fd.is_valid());

  DeleteCgroup(child_path);

  std::string pid_string = std::to_string(getpid());
  EXPECT_THAT(write(child_procs_fd.get(), pid_string.c_str(), pid_string.length()),
              SyscallFailsWithErrno(ENODEV));
}
