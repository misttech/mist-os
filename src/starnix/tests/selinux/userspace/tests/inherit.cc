// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/time.h>

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

// Try to execute a binary in a situation where the post-exec domain does not
// have the `use` permission for file descriptors opened in the pre-exec domain.
// On Linux, the executed program segfaults.
// TODO: https://fxbug.dev/322843830 - On Starnix, the executed program exits normally.
TEST(InheritTest, ExecutableFdRemappedToNull) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_no_use_fd_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  pid_t pid;
  ASSERT_TRUE((pid = fork()) >= 0);
  if (pid == 0) {
    auto set_context = WriteTaskAttr("current", kParentSecurityContext);
    ASSERT_TRUE(set_context.is_ok());

    auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
    ASSERT_TRUE(set_exec_context.is_ok());

    std::string path_for_exec = "data/bin/true_bin";
    char* args[] = {basename(path_for_exec.data()), NULL};
    if (execv(path_for_exec.data(), args) < 0) {
      perror("exec into child domain failed");
      FAIL();
    }
  } else {
    int wstatus;
    ASSERT_TRUE(waitpid(pid, &wstatus, 0));
    EXPECT_TRUE(WIFSIGNALED(wstatus));
    EXPECT_EQ(WTERMSIG(wstatus), SIGSEGV);
  }
}

// Execute a binary in a situation where the post-exec domain has the `use`
// permission for file descriptors opened by the pre-exec domain. The executed
// program should exit normally.
TEST(InheritTest, ExecutableFdUseAllowed) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_allow_use_fd_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
    ASSERT_TRUE(set_exec_context.is_ok());

    std::string path_for_exec = "data/bin/true_bin";
    char* args[] = {basename(path_for_exec.data()), NULL};
    if (execv(path_for_exec.data(), args) < 0) {
      perror("exec into child domain failed");
      FAIL();
    }
  }));
}

// Under the parent domain, open a test file such that the child domain does not have the
// `fd { use }` permission on the file descriptor. Then exec into the child domain via an
// intermediate domain. The child program checks that the test file descriptor was remapped
// to the null file.
TEST(InheritTest, FdUseDeniedFdRemappedToNull) {
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

// Under the parent domain, open a test file twice such that the child domain does not have the
// `fd { use }` permission on the file descriptor, so that the two open file descriptors should
// be remapped to the selinuxfs null node during exec. Then exec into the child domain via an
// intermediate domain. The child program checks that the two file descriptors are duplicates:
// they have independent file descriptor flag state, but refer to the same file description.
TEST(InheritTest, NullFileDescriptorIsDuplicated) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kBridgeSecurityContext[] = "test_u:test_r:test_inherit_bridge_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_no_use_fd_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    auto tmp_file_path = CreateTmpFile();
    ASSERT_TRUE(tmp_file_path.is_ok());

    int no_use_fd_1 = open(tmp_file_path.value().data(), O_RDONLY);
    ASSERT_TRUE(no_use_fd_1 >= 0);
    std::string no_use_fd_1_str = std::to_string(no_use_fd_1);

    int no_use_fd_2 = open(tmp_file_path.value().data(), O_RDONLY);
    ASSERT_TRUE(no_use_fd_2 >= 0);
    std::string no_use_fd_2_str = std::to_string(no_use_fd_2);

    ASSERT_TRUE(RunSubprocessAs(kBridgeSecurityContext, [&] {
      // Exec the `is_duplicated_fd` binary and expect that `no_use_fd_1` and
      // `no_use_fd_2` are remapped to the same file description (for the null node).
      std::string path_for_exec = "data/bin/is_duplicated_fd_bin";
      std::string expect_null_inode = std::to_string(int(true));
      char* args[] = {basename(path_for_exec.data()), no_use_fd_1_str.data(),
                      no_use_fd_2_str.data(), NULL};

      auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
      ASSERT_TRUE(set_exec_context.is_ok());

      if (execv(path_for_exec.data(), args) < 0) {
        perror("exec into child domain failed");
        FAIL();
      }
    }));
  }));
}

// Under the parent domain, open a test file such that the child domain does has the `fd { use }`
// permission on the file descriptor, but does not have the `read` permission on the file. Then exec
// into the child domain. The child program checks that the test file descriptor was remapped to the
// null file.
TEST(InheritTest, FsNodePermissionDeniedFdRemappedToNull) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_no_read_file_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    auto tmp_file_path = CreateTmpFile();
    ASSERT_TRUE(tmp_file_path.is_ok());
    int no_use_fd = open(tmp_file_path.value().data(), O_RDONLY);
    ASSERT_TRUE(no_use_fd >= 0);
    std::string no_use_fd_str = std::to_string(no_use_fd);

    // Exec the `is_selinux_null_inode` binary and expect that `no_use_fd` is remapped.
    std::string path_for_exec = "data/bin/is_selinux_null_inode_bin";
    std::string expect_null_inode = std::to_string(int(true));
    char* args[] = {basename(path_for_exec.data()), no_use_fd_str.data(), expect_null_inode.data(),
                    NULL};

    auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
    ASSERT_TRUE(set_exec_context.is_ok());

    if (execv(path_for_exec.data(), args) < 0) {
      perror("exec into child domain failed");
      FAIL();
    }
  }));
}

// Under the parent domain, open a test file such that the child domain has the `fd { use }`
// permission on the file descriptor and has the appropriate file class permissions on the file.
// Then exec into the child domain. The child program checks that the test file descriptor was
// not remapped to the null file.
TEST(InheritTest, FdUseAllowed) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_allow_use_fd_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    auto tmp_file_path = CreateTmpFile();
    ASSERT_TRUE(tmp_file_path.is_ok());
    int allow_use_fd = open(tmp_file_path.value().data(), O_RDONLY);
    ASSERT_TRUE(allow_use_fd >= 0);
    std::string allow_use_fd_str = std::to_string(allow_use_fd);

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
}

// When the `siginh` permission is denied, the parent's ITIMER_REAL is reset during `exec`.
TEST(InheritTest, SiginhDeniedItimerRealReset) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_no_siginh_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    struct itimerval parent_val;
    parent_val.it_value.tv_sec = 1000000;
    parent_val.it_value.tv_usec = 0;
    parent_val.it_interval.tv_sec = 0;
    parent_val.it_interval.tv_usec = 0;

    ASSERT_THAT(setitimer(ITIMER_REAL, &parent_val, nullptr), SyscallSucceeds());

    std::string path_for_exec = "data/bin/is_itimer_real_reset_bin";
    std::string expect_itimer_real_reset = std::to_string(int(true));
    char* args[] = {basename(path_for_exec.data()), expect_itimer_real_reset.data(), NULL};

    auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
    ASSERT_TRUE(set_exec_context.is_ok());

    if (execv(path_for_exec.data(), args) < 0) {
      perror("exec into child domain failed");
      FAIL();
    }
  }));
}

// When the `siginh` permission is allowed, the parent's ITIMER_REAL is preserved across `exec`.
TEST(InheritTest, SiginhAllowedItimerRealInherited) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_inherit_parent_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_inherit_child_allow_siginh_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    struct itimerval parent_val;
    parent_val.it_value.tv_sec = 1000000;
    parent_val.it_value.tv_usec = 0;
    parent_val.it_interval.tv_sec = 0;
    parent_val.it_interval.tv_usec = 0;

    ASSERT_THAT(setitimer(ITIMER_REAL, &parent_val, nullptr), SyscallSucceeds());

    std::string path_for_exec = "data/bin/is_itimer_real_reset_bin";
    std::string expect_itimer_real_reset = std::to_string(int(false));
    char* args[] = {basename(path_for_exec.data()), expect_itimer_real_reset.data(), NULL};

    auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
    ASSERT_TRUE(set_exec_context.is_ok());

    if (execv(path_for_exec.data(), args) < 0) {
      perror("exec into child domain failed");
      FAIL();
    }
  }));
}

}  // namespace
