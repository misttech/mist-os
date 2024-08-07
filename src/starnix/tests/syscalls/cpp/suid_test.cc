// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <grp.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mount.h>
#include <unistd.h>

#include <filesystem>
#include <optional>

#include <linux/capability.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

constexpr int kOutputFd = 100;

constexpr uid_t kUser1Uid = 65533;
constexpr gid_t kUser1Gid = 65534;
constexpr uid_t kRootUid = 0;
constexpr gid_t kRootGid = 0;

std::string GetCredsBinaryPath() {
  std::string test_binary = "/data/tests/suid_test_exec_child";
  if (!files::IsFile(test_binary)) {
    // We're running on host
    char self_path[PATH_MAX];
    realpath("/proc/self/exe", self_path);

    test_binary = files::JoinPath(files::GetDirectoryName(self_path), "suid_test_exec_child");
  }
  return test_binary;
}

bool change_ids(uid_t user, gid_t group) {
  return (setresgid(group, group, group) == 0) && (setresuid(user, user, user) == 0);
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

}  // namespace

TEST(SuidTest, SuidBinaryBecomesRoot) {
  if (!test_helper::HasSysAdmin()) {
    GTEST_SKIP() << "Not running with sysadmin capabilities, skipping.";
  }
  std::string creds_binary = GetCredsBinaryPath();

  test_helper::ScopedTempDir temp_dir;
  auto mounted = MountTmpFs(temp_dir.path());
  ASSERT_TRUE(mounted.has_value()) << "failed to mount fs";
  auto mount_path = mounted.value();

  // Directory Permissions: owner can do everything, user and other can search.
  constexpr int kDirPerms = S_IRWXU | S_IXGRP | S_IXOTH;

  SAFE_SYSCALL(chmod(mount_path.c_str(), kDirPerms));
  SAFE_SYSCALL(chmod(temp_dir.path().c_str(), kDirPerms));

  std::string suid_binary_path = files::JoinPath(mount_path, "suid_binary_becomes_root");
  std::filesystem::copy_file(creds_binary, suid_binary_path);

  // File permissions: A set-user-ID binary that can be executed by Others.
  constexpr int kBinaryPerms = S_ISUID | S_IXOTH;
  SAFE_SYSCALL(chown(suid_binary_path.c_str(), kRootUid, kRootGid));
  SAFE_SYSCALL(chmod(suid_binary_path.c_str(), kBinaryPerms));

  // Binary will output a string to this memfd.
  int fd = SAFE_SYSCALL(test_helper::MemFdCreate("creds", O_RDWR));
  pid_t pid = SAFE_SYSCALL(fork());
  if (pid == 0) {
    ASSERT_TRUE(fcntl(kOutputFd, F_GETFD) == -1 && errno == EBADF);
    SAFE_SYSCALL(dup2(fd, kOutputFd));

    SAFE_SYSCALL(setgroups(0, nullptr));  // drop all supplementary groups.
    ASSERT_TRUE(change_ids(kUser1Uid, kUser1Gid));

    char *const argv[] = {const_cast<char *>(suid_binary_path.c_str()), nullptr};

    SAFE_SYSCALL(execve(suid_binary_path.c_str(), argv, nullptr));
    _exit(EXIT_FAILURE);
  }

  int status = 0;
  SAFE_SYSCALL(waitpid(pid, &status, 0));
  EXPECT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0);

  SAFE_SYSCALL(lseek(fd, 0, SEEK_SET));
  FILE *fp = fdopen(fd, "r");

  {
    uid_t ruid, euid, suid;
    EXPECT_EQ(fscanf(fp, "ruid: %u euid: %u suid: %u\n", &ruid, &euid, &suid), 3);
    EXPECT_EQ(ruid, kUser1Uid);
    EXPECT_EQ(euid, kRootUid);
    EXPECT_EQ(suid, kRootUid);
  }

  {
    gid_t rgid, egid, sgid;
    EXPECT_EQ(fscanf(fp, "rgid: %u egid: %u sgid: %u\n", &rgid, &egid, &sgid), 3);
    EXPECT_EQ(rgid, kUser1Gid);
    EXPECT_EQ(egid, kUser1Gid);
    EXPECT_EQ(sgid, kUser1Gid);
  }

  fclose(fp);
}
