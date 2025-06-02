// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/starnix/tests/selinux/userspace/util.h"

extern std::string DoPrePolicyLoadWork() { return "inherit_policy.pp"; }

namespace {

constexpr char kTmpFilePathTemplate[] = "/tmp/inherit_test_file:XXXXXX";

fit::result<int, std::string> CreateTmpFile() {
  std::string file_path(kTmpFilePathTemplate);
  fbl::unique_fd fd(mkstemp(file_path.data()));
  if (!fd.is_valid()) {
    return fit::error(errno);
  }
  return fit::ok(std::move(file_path));
}

// Under the parent domain, open a test file such that the child domain does not have the
// `fd { use }` permission on the file descriptor. Then exec into the child domain via an
// intermediate domain. The child program checks that the test file descriptor was remapped
// to the null file.
TEST(InheritTest, FdRemappedToNull) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kBridgeSecurityContext[] = "test_u:test_r:test_inherit_bridge_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_no_use_fd_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    auto tmp_file_path = CreateTmpFile();
    ASSERT_TRUE(tmp_file_path.is_ok());
    int no_use_fd = open(tmp_file_path.value().data(), O_RDONLY);
    ASSERT_TRUE(no_use_fd >= 0);
    std::string no_use_fd_str = std::to_string(no_use_fd);

    ASSERT_TRUE(RunSubprocessAs(kBridgeSecurityContext, [&] {
      // Exec the `is_selinux_null_inode` binary and expect that `no_use_fd` is remapped.
      std::string path_for_exec = "data/bin/is_selinux_null_inode_bin";
      std::string expect_null_inode = std::to_string(int(true));
      char* args[] = {basename(path_for_exec.data()), no_use_fd_str.data(),
                      expect_null_inode.data(), NULL};

      auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
      ASSERT_TRUE(set_exec_context.is_ok());

      if (execv(path_for_exec.data(), args) < 0) {
        perror("exec into child domain failed");
        FAIL();
      }
    }));
  }));
}

// Under the parent domain, open a test file such that the child domain has the `fd { use }`
// permission on the file descriptor. Then exec into the child domain via an intermediate domain.
// The child program checks that the test file descriptor was not remapped to the null file.
TEST(InheritTest, FdUseAllowed) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kBridgeSecurityContext[] = "test_u:test_r:test_inherit_bridge_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_allow_use_fd_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    auto tmp_file_path = CreateTmpFile();
    ASSERT_TRUE(tmp_file_path.is_ok());
    int allow_use_fd = open(tmp_file_path.value().data(), O_RDONLY);
    ASSERT_TRUE(allow_use_fd >= 0);
    std::string allow_use_fd_str = std::to_string(allow_use_fd);

    ASSERT_TRUE(RunSubprocessAs(kBridgeSecurityContext, [&] {
      // Exec the `is_selinux_null_inode` binary and expect that `allow_use_fd` is not remapped.
      std::string path_for_exec = "data/bin/is_selinux_null_inode_bin";
      std::string expect_null_inode = std::to_string(int(false));
      char* args[] = {basename(path_for_exec.data()), allow_use_fd_str.data(),
                      expect_null_inode.data(), NULL};

      auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
      ASSERT_TRUE(set_exec_context.is_ok());

      if (execv(path_for_exec.data(), args) < 0) {
        perror("exec into child domain failed");
        FAIL();
      }
    }));
  }));
}

}  // namespace
