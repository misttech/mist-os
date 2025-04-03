// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <mntent.h>
#include <sys/mount.h>
#include <unistd.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"

namespace {

std::string MountOptionsFor(std::string_view mount_path) {
  FILE* mounts = setmntent("/proc/mounts", "r");
  std::string result;
  for (struct mntent* entry = 0; (entry = getmntent(mounts));) {
    if (mount_path == entry->mnt_dir) {
      result = entry->mnt_opts;
      break;
    }
  }
  endmntent(mounts);
  return result;
}

TEST(MountTest, NoSelinuxMountOptions) {
  LoadPolicy("minimal_policy.pp");

  ASSERT_THAT(mkdir("/mount_tests", 0755), SyscallSucceeds());
  ASSERT_THAT(mount("tmpfs", "/mount_tests", "tmpfs", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
              SyscallSucceeds());

  std::string mount_options = MountOptionsFor("/mount_tests");
  ASSERT_THAT(umount("/mount_tests"), SyscallSucceeds());
  ASSERT_THAT(rmdir("/mount_tests"), SyscallSucceeds());

  EXPECT_NE(mount_options.find("nosuid"), std::string::npos);
  EXPECT_NE(mount_options.find("noexec"), std::string::npos);
  EXPECT_NE(mount_options.find("nodev"), std::string::npos);
}

TEST(MountTest, WithContextOption) {
  LoadPolicy("minimal_policy.pp");

  ASSERT_THAT(mkdir("/mount_tests", 0755), SyscallSucceeds());
  ASSERT_THAT(mount("tmpfs", "/mount_tests", "tmpfs", MS_NOEXEC | MS_NOSUID | MS_NODEV,
                    "context=source_u:object_r:target_t:s0"),
              SyscallSucceeds());

  std::string mount_options = MountOptionsFor("/mount_tests");
  ASSERT_THAT(umount("/mount_tests"), SyscallSucceeds());
  ASSERT_THAT(rmdir("/mount_tests"), SyscallSucceeds());

  EXPECT_EQ(mount_options.find("seclabel"), std::string::npos);
  EXPECT_NE(mount_options.find("context="), std::string::npos);
}

TEST(MountTest, WithSeclabel) {
  LoadPolicy("minimal_policy.pp");

  // Base policy uses "fs_use_trans" labeling scheme for "tmpfs", which should report "seclabel".
  ASSERT_THAT(mkdir("/with_seclabel", 0755), SyscallSucceeds());
  ASSERT_THAT(mount("tmpfs", "/with_seclabel", "tmpfs", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
              SyscallSucceeds());

  std::string mount_options = MountOptionsFor("/with_seclabel");
  ASSERT_THAT(umount("/with_seclabel"), SyscallSucceeds());
  ASSERT_THAT(rmdir("/with_seclabel"), SyscallSucceeds());

  EXPECT_NE(mount_options.find("seclabel"), std::string::npos);
}

TEST(MountTest, WithoutSeclabel) {
  LoadPolicy("minimal_policy.pp");

  // Base policy uses "genfscon" labeling scheme for "proc", which should not report "seclabel".
  ASSERT_THAT(mkdir("/without_seclabel", 0755), SyscallSucceeds());
  ASSERT_THAT(
      mount("selinuxfs", "/without_seclabel", "selinuxfs", MS_NOEXEC | MS_NOSUID | MS_NODEV, 0),
      SyscallSucceeds());

  std::string mount_options = MountOptionsFor("/without_seclabel");
  ASSERT_THAT(umount("/without_seclabel"), SyscallSucceeds());
  ASSERT_THAT(rmdir("/without_seclabel"), SyscallSucceeds());

  EXPECT_EQ(mount_options.find("seclabel"), std::string::npos);
}

}  // namespace
