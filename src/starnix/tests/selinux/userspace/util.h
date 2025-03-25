// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_TESTS_SELINUX_USERSPACE_UTIL_H_
#define SRC_STARNIX_TESTS_SELINUX_USERSPACE_UTIL_H_

#include <lib/fit/result.h>
#include <string.h>

#include <string>

#include <gmock/gmock.h>

/// Loads the policy |name|.
void LoadPolicy(const std::string& name);

/// Atomically writes |contents| to |file|, and fails the test otherwise.
void WriteContents(const std::string& file, const std::string& contents, bool create = false);

/// Reads |file|, or fail the test.
std::string ReadFile(const std::string& file);

/// Reads the specified security attribute (e.g. "current", "exec", etc) for the current task.
fit::result<int, std::string> ReadTaskAttr(std::string_view attr_name);

/// Returns the `in`put string with the trailing NUL character, if any, removed.
/// Some SELinux surfaces (e.g. "/proc/<pid/attr/<attr>") include the terminating NUL in the
/// returned content under Linux, but not under SEStarnix.
std::string RemoveTrailingNul(std::string in);

/// Reads the security label of the specified `fd`, returning the `errno` on failure.
/// The trailing NUL, if any, will be stripped before the label is returned.
fit::result<int, std::string> GetLabel(int fd);

/// Runs the given action in a forked process after transitioning to |label|. This requires some
/// rules to be set-up. For transitions from unconfined_t (the starting label for tests), giving
/// them the `test_a` attribute from `test_policy.conf` is sufficient.
template <typename T>
::testing::AssertionResult RunAs(const std::string& label, T action) {
  pid_t pid;
  if ((pid = fork()) == 0) {
    WriteContents("/proc/thread-self/attr/current", label.c_str());
    action();
    _exit(testing::Test::HasFailure());
  }
  if (pid == -1) {
    return ::testing::AssertionFailure() << "fork failed: " << strerror(errno);
  } else {
    int wstatus;
    pid_t ret = waitpid(pid, &wstatus, 0);
    if (ret == -1) {
      return ::testing::AssertionFailure() << "waitpid failed: " << strerror(errno);
    }
    if (!WIFEXITED(wstatus) || WEXITSTATUS(wstatus) != 0) {
      return ::testing::AssertionFailure()
             << "forked process existed with status: " << WEXITSTATUS(wstatus) << " and signal "
             << WTERMSIG(wstatus);
    }
    return ::testing::AssertionSuccess();
  }
}

/// Enables (or disables) enforcement while in scope, then restores enforcement to the previous
/// state.
class ScopedEnforcement {
 public:
  static ScopedEnforcement SetEnforcing();
  static ScopedEnforcement SetPermissive();
  ~ScopedEnforcement();

 private:
  explicit ScopedEnforcement(bool enforcing);
  std::string previous_state_;
};

MATCHER_P(IsOk, expected_value, std::string("fit::result<> is fit::ok(") + expected_value + ")") {
  if (arg.is_error()) {
    *result_listener << "failed with error: " << arg.error_value();
    return false;
  }
  ::testing::Matcher<fit::result<int, std::string>> expected = ::testing::Eq(expected_value);
  return expected.MatchAndExplain(arg, result_listener);
}

MATCHER(SyscallSucceeds, "syscall succeeds") {
  if (arg != -1) {
    return true;
  }
  *result_listener << "syscall failed with error " << strerror(errno);
  return false;
}

MATCHER_P(SyscallFailsWithErrno, expected_errno,
          std::string("syscall fails with error ") + strerror(expected_errno)) {
  if (arg != -1) {
    *result_listener << "syscall succeeded";
    return false;
  } else if (errno == expected_errno) {
    return true;
  } else {
    *result_listener << "syscall failed with error " << strerror(errno);
    return false;
  }
}

MATCHER_P(FdIsLabeled, expected_label, std::string("fd is labeled with ") + expected_label) {
  if (arg < 0) {
    *result_listener << "invalid fd";
    return false;
  }
  ::testing::Matcher<fit::result<int, std::string>> expected =
      IsOkMatcherP<std::string>(expected_label);
  return expected.MatchAndExplain(GetLabel(arg), result_listener);
}

namespace fit {

/// Kludge to tell gTest how to stringify `fit::result<>` values.
template <typename E, typename T>
void PrintTo(const fit::result<E, T>& result, std::ostream* os) {
  if (result.is_error()) {
    *os << "fit::failed( " << result.error_value() << " )";
  } else {
    *os << "fit::ok( " << result.value() << " )";
  }
}

}  // namespace fit

#endif  // SRC_STARNIX_TESTS_SELINUX_USERSPACE_UTIL_H_
