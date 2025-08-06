// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/ptrace.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

extern std::string DoPrePolicyLoadWork() { return "ptrace_policy.pp"; }

namespace {

// Returns the path to a binary under the test package's `data` directory.
std::string PathForExec(std::string_view binary_name) {
  return "data/bin/" + std::string(binary_name);
}

// When the `ptrace` permission is denied to the parent for the child task, a
// `ptrace(PTRACE_TRACEME,...)` call by the child should fail with EACCES and
// the child program should exit normally in the absence of other errors.
TEST(PtraceTest, PtraceTraceMeDenied) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_ptrace_parent_deny_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_ptrace_child_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    pid_t pid;
    ASSERT_TRUE((pid = fork()) >= 0);
    if (pid == 0) {
      auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
      ASSERT_TRUE(set_exec_context.is_ok());

      std::string binary_name = "ptrace_traceme_bin";
      std::string path_for_exec = PathForExec(binary_name);
      std::string expect_success = std::to_string(false);
      char* const args[] = {binary_name.data(), expect_success.data(), NULL};
      SAFE_SYSCALL(execv(path_for_exec.data(), args));
    } else {
      int wstatus;
      ASSERT_THAT(waitpid(pid, &wstatus, 0), SyscallSucceeds());
      EXPECT_TRUE(WIFEXITED(wstatus));
      EXPECT_EQ(WEXITSTATUS(wstatus), 0);
    }
  }));
}

// When the `ptrace` permission is granted to the parent for the child task, a
// `ptrace(PTRACE_TRACEME,...)` call by the child should succeed.
TEST(PtraceTest, PtraceTraceMeAllowed) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_ptrace_parent_allow_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_ptrace_child_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    pid_t pid;
    ASSERT_TRUE((pid = fork()) >= 0);
    if (pid == 0) {
      auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
      ASSERT_TRUE(set_exec_context.is_ok());

      std::string binary_name = "ptrace_traceme_bin";
      std::string path_for_exec = PathForExec(binary_name);
      std::string expect_success = std::to_string(true);
      char* const args[] = {binary_name.data(), expect_success.data(), NULL};
      SAFE_SYSCALL(execv(path_for_exec.data(), args));
    } else {
      int wstatus;
      ASSERT_THAT(waitpid(pid, &wstatus, 0), SyscallSucceeds());
      EXPECT_TRUE(WIFEXITED(wstatus));
      EXPECT_EQ(WEXITSTATUS(wstatus), 0);
    }
  }));
}

// When the `ptrace` permission is denied to the parent for the child task, a
// `ptrace(PTRACE_ATTACH,...)` call by the parent should fail with EACCES and the
// child program should exit normally in the absence of other errors.
TEST(PtraceTest, PtraceAttachDenied) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_ptrace_parent_deny_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_ptrace_child_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    pid_t pid;
    ASSERT_TRUE((pid = fork()) >= 0);
    if (pid == 0) {
      // Exec into the child domain.
      auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
      ASSERT_TRUE(set_exec_context.is_ok());

      std::string binary_name = "stop_bin";
      std::string path_for_exec = PathForExec(binary_name);
      char* const args[] = {binary_name.data(), NULL};
      SAFE_SYSCALL(execv(path_for_exec.data(), args));
    } else {
      // Wait for the child program to stop, then attempt to attach (expecting failure).
      int wstatus;
      ASSERT_THAT(waitpid(pid, &wstatus, WUNTRACED), SyscallSucceeds());
      ASSERT_TRUE(WIFSTOPPED(wstatus));
      EXPECT_THAT(ptrace(PTRACE_ATTACH, pid, nullptr, nullptr), SyscallFailsWithErrno(EACCES));

      // Continue the child program and check that it exits normally.
      int continue_result = kill(pid, SIGCONT);
      ASSERT_EQ(continue_result, 0);
      if (continue_result != 0) {
        SAFE_SYSCALL(kill(pid, SIGKILL));
      }
      bool exited = false;
      ASSERT_THAT(waitpid(pid, &wstatus, WUNTRACED), SyscallSucceeds());
      exited = WIFEXITED(wstatus);
      EXPECT_TRUE(exited);
      if (exited) {
        EXPECT_EQ(WEXITSTATUS(wstatus), 0);
      } else {
        SAFE_SYSCALL(kill(pid, SIGKILL));
      }
    }
  }));
}

// When the `ptrace` permission is granted to the parent for the child task, a
// `ptrace(PTRACE_ATTACH,...)` call by the parent should succeed.
TEST(PtraceTest, PtraceAttachAllowed) {
  constexpr char kParentSecurityContext[] = "test_u:test_r:test_ptrace_parent_allow_t:s0";
  constexpr char kChildSecurityContext[] = "test_u:test_r:test_ptrace_child_t:s0";

  auto enforce = ScopedEnforcement::SetEnforcing();

  ASSERT_TRUE(RunSubprocessAs(kParentSecurityContext, [&] {
    pid_t pid;
    ASSERT_TRUE((pid = fork()) >= 0);
    if (pid == 0) {
      // Exec into the child domain.
      auto set_exec_context = WriteTaskAttr("exec", kChildSecurityContext);
      ASSERT_TRUE(set_exec_context.is_ok());

      std::string binary_name = "stop_bin";
      std::string path_for_exec = PathForExec(binary_name);
      char* const args[] = {binary_name.data(), NULL};
      SAFE_SYSCALL(execv(path_for_exec.data(), args));
    } else {
      // Wait for the child program to stop, then attempt to attach (expecting success).
      int wstatus;
      ASSERT_THAT(waitpid(pid, &wstatus, WUNTRACED), SyscallSucceeds());
      ASSERT_TRUE(WIFSTOPPED(wstatus));
      EXPECT_THAT(ptrace(PTRACE_ATTACH, pid, nullptr, nullptr), SyscallSucceeds());

      // Continue the child program and check that it exits normally.
      // This requires 2 `PTRACE_CONT` commands, one to resume from the `PTRACE_ATTACH`
      // command and one to resume from the child's self-signaled `SIGSTOP`.
      EXPECT_THAT(ptrace(PTRACE_CONT, pid, nullptr, 0), SyscallSucceeds());
      ASSERT_THAT(waitpid(pid, &wstatus, 0), SyscallSucceeds());
      ASSERT_TRUE(WIFSTOPPED(wstatus));

      EXPECT_THAT(ptrace(PTRACE_CONT, pid, nullptr, 0), SyscallSucceeds());
      bool exited = false;
      ASSERT_THAT(waitpid(pid, &wstatus, 0), SyscallSucceeds());
      exited = WIFEXITED(wstatus);
      EXPECT_TRUE(exited);
      if (exited) {
        EXPECT_EQ(WEXITSTATUS(wstatus), 0);
      } else {
        SAFE_SYSCALL(kill(pid, SIGKILL));
      }
    }
  }));
}

}  // namespace
