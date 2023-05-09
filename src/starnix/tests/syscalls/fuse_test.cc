// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_TESTS_SYSCALLS_PROC_TEST_H_
#define SRC_STARNIX_TESTS_SYSCALLS_PROC_TEST_H_

#include <fcntl.h>
#include <sys/mount.h>
#include <sys/stat.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/syscall_matchers.h"
#include "src/starnix/tests/syscalls/test_helper.h"

#define OK_OR_RETURN(x) \
  {                     \
    auto result = (x);  \
    if (!result) {      \
      return result;    \
    }                   \
  }

constexpr char kOverlayFsPath[] = "OVERLAYFS_PATH";

class FuseTest : public ::testing::Test {
 public:
  void SetUp() override {}

  void TearDown() override {
    if (mount_dir_) {
      if (umount(mount_dir_->c_str()) != 0) {
        FAIL() << "Unable to umount: " << strerror(errno);
      }
      mount_dir_.reset();
    }
  }

 protected:
  std::string GetOverlayFsPath() {
    if (getenv(kOverlayFsPath)) {
      return getenv(kOverlayFsPath);
    } else {
      return "/data/fuse-overlayfs";
    }
  }

  testing::AssertionResult MkDir(const std::string& directory) {
    if (mkdir(directory.c_str(), 0700) != 0) {
      return testing::AssertionFailure()
             << "Unable to create '" << directory << "': " << strerror(errno);
    }
    return testing::AssertionSuccess();
  }

  testing::AssertionResult Mount() {
    if (getuid() != 0) {
      return testing::AssertionFailure()
             << "Unable to run without CAP_SYS_ADMIN (please run as root)";
    }
    if (access(GetOverlayFsPath().c_str(), R_OK | X_OK) != 0) {
      return testing::AssertionFailure()
             << "Unable to find fuse binary at: " << GetOverlayFsPath() << "(set OVERLAYFS_PATH)";
    }

    std::string base_dir = "/tmp/fuse_base_dir_XXXXXX";
    if (mkdtemp(const_cast<char*>(base_dir.c_str())) == nullptr) {
      return testing::AssertionFailure()
             << "Unable to create temporary directory: " << strerror(errno);
    }
    std::string lowerdir = base_dir + "/lower";
    OK_OR_RETURN(MkDir(lowerdir));
    std::string upperdir = base_dir + "/upper";
    OK_OR_RETURN(MkDir(upperdir));
    std::string workdir = base_dir + "/work";
    OK_OR_RETURN(MkDir(workdir));
    std::string mergedir = base_dir + "/merge";
    OK_OR_RETURN(MkDir(mergedir));
    std::string witness_name = "witness";
    {
      ScopedFD witness(open((lowerdir + "/" + witness_name).c_str(), O_RDONLY | O_CREAT, 0600));
      if (!witness.is_valid()) {
        return testing::AssertionFailure() << "Unable to create witness file: " << strerror(errno);
      }
    }
    fork_helper_.RunInForkedProcess([&] {
      std::string configuration =
          "lowerdir=" + lowerdir + ",upperdir=" + upperdir + ",workdir=" + workdir;
      execl(GetOverlayFsPath().c_str(), GetOverlayFsPath().c_str(), "-f", "-o",
            configuration.c_str(), mergedir.c_str(), NULL);
    });
    std::string witness = mergedir + "/" + witness_name;
    for (int i = 0; i < 20 && access(witness.c_str(), R_OK) != 0; ++i) {
      usleep(100000);
    }
    if (access(witness.c_str(), R_OK) != 0) {
      return testing::AssertionFailure() << "Unable to see witness file. Mount failed?";
    }
    mount_dir_ = mergedir;
    return testing::AssertionSuccess();
  }

  ForkHelper fork_helper_;
  std::optional<std::string> mount_dir_;
};

TEST_F(FuseTest, Mount) { ASSERT_TRUE(Mount()); }

#endif  // SRC_STARNIX_TESTS_SYSCALLS_PROC_TEST_H_
