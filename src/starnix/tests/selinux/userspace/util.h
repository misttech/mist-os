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

/// Writes `data` to the file at `path`, returning the `errno` if any part of that process fails.
fit::result<int> WriteExistingFile(const std::string& path, std::string_view data);

/// Reads the contents of the file at `path`.
fit::result<int, std::string> ReadFile(const std::string_view& path);

/// Reads the specified security attribute (e.g. "current", "exec", etc) for the current task.
fit::result<int, std::string> ReadTaskAttr(std::string_view attr_name);

/// Writes the specified security attribute (e.g. "current", "exec", etc) for the current task.
fit::result<int> WriteTaskAttr(std::string_view attr_name, std::string_view context);

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
::testing::AssertionResult RunSubprocessAs(std::string_view label, T action) {
  pid_t pid;
  if ((pid = fork()) == 0) {
    if (WriteTaskAttr("current", label).is_error()) {
      _exit(1);
    }
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
             << "forked process exited with status: " << WEXITSTATUS(wstatus) << " and signal "
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

template <typename E, typename T>
bool operator==(const fit::result<E, T>& result, const fit::error<E>& expected) {
  return result == fit::result<E, T>(expected);
}

template <typename E, typename T, typename T2>
bool operator==(const fit::result<E, T>& result, const fit::success<T2>& expected) {
  return result == fit::result<E, T>(expected);
}

}  // namespace fit

#endif  // SRC_STARNIX_TESTS_SELINUX_USERSPACE_UTIL_H_
