// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_STARNIX_TESTS_SYSCALLS_TEST_HELPER_H_
#define SRC_STARNIX_TESTS_SYSCALLS_TEST_HELPER_H_

#include <unistd.h>

#include <functional>
#include <optional>
#include <string_view>

#include "syscall_matchers.h"

#define SAFE_SYSCALL(X)                                                             \
  ({                                                                                \
    auto retval = (X);                                                              \
    if (retval < 0) {                                                               \
      ADD_FAILURE() << #X << " failed: " << strerror(errno) << "(" << errno << ")"; \
      _exit(-1);                                                                    \
    }                                                                               \
    retval;                                                                         \
  })

#define SAFE_SYSCALL_SKIP_ON_EPERM(X)                                                 \
  ({                                                                                  \
    auto retval = (X);                                                                \
    if (retval < 0 && errno == EPERM) {                                               \
      if (errno == EPERM) {                                                           \
        GTEST_SKIP() << "Permission denied for " << #X << ", skipping tests.";        \
      } else {                                                                        \
        ADD_FAILURE() << #X << " failed: " << strerror(errno) << "(" << errno << ")"; \
        _exit(-1);                                                                    \
      }                                                                               \
    }                                                                                 \
    retval;                                                                           \
  })

// Helper class to handle test that needs to fork and do assertion on the child
// process.
class ForkHelper {
 public:
  ForkHelper();
  ~ForkHelper();

  // Wait for all children of the current process, and return true if all exited
  // with a 0 status.
  bool WaitForChildren();

  // For the current process and execute the given |action| inside the child,
  // then exit with a status equals to the number of failed expectation and
  // assertion. Return immediately with the pid of the child.
  pid_t RunInForkedProcess(std::function<void()> action);

  // Checks for process termination by the given signal, instead of expecting
  // the forked process to terminate normally.
  void ExpectSignal(int signum);

 private:
  int death_signum_;
};

// Helper class to handle tests that needs to clone processes.
class CloneHelper {
 public:
  CloneHelper();
  ~CloneHelper();

  // Call clone with the specified childFunction and cloneFlags.
  // Perform the necessary asserts to ensure the clone was performed with
  // no errors and return the new process ID.
  int runInClonedChild(unsigned int cloneFlags, int (*childFunction)(void *));

  // Handy trivial function for passing clone when we want the child to
  // sleep for 1 second and return 0.
  static int sleep_1sec(void *);

  // Handy trivial function for passing clone when we want the child to
  // do nothing and return 0.
  static int doNothing(void *);

 private:
  uint8_t *_childStack;
  uint8_t *_childStackBegin;
  static constexpr size_t _childStackSize = 0x5000;
};

// Helper class to modify signal masks of processes
class SignalMaskHelper {
 public:
  // Blocks the specified signal and saves the original signal mask
  // to _sigmaskCopy.
  void blockSignal(int signal);

  // Blocks the execution until the specified signal is received.
  void waitForSignal(int signal);

  // Sets the signal mask of the process with _sigmaskCopy.
  void restoreSigmask();

 private:
  sigset_t _sigset;
  sigset_t _sigmaskCopy;
};

class ScopedFD {
 public:
  explicit ScopedFD(int fd = -1) : fd_(fd) {}
  ScopedFD(ScopedFD &&other) noexcept {
    fd_ = -1;
    *this = std::move(other);
  }
  ~ScopedFD() {
    if (is_valid())
      close(fd_);
  }

  ScopedFD &operator=(ScopedFD &&other) noexcept {
    reset();
    fd_ = other.fd_;
    other.fd_ = -1;
    return *this;
  }

  bool is_valid() const { return fd_ != -1; }
  void reset() {
    if (is_valid()) {
      close(fd_);
    }
    fd_ = -1;
  }
  explicit operator bool() const { return is_valid(); }

  int get() const { return fd_; }

 private:
  int fd_;
};

class ScopedTempFD {
 public:
  ScopedTempFD();
  ~ScopedTempFD() { unlink(name_.c_str()); }

  bool is_valid() const { return fd_.is_valid(); }
  explicit operator bool() const { return is_valid(); }

  const std::string &name() const { return name_; }
  int fd() const { return fd_.get(); }

 public:
  std::string name_;
  ScopedFD fd_;
};

#define HANDLE_EINTR(x)                                     \
  ({                                                        \
    decltype(x) eintr_wrapper_result;                       \
    do {                                                    \
      eintr_wrapper_result = (x);                           \
    } while (eintr_wrapper_result == -1 && errno == EINTR); \
    eintr_wrapper_result;                                   \
  })

void waitForChildSucceeds(unsigned int waitFlag, int cloneFlags, int (*childRunFunction)(void *),
                          int (*parentRunFunction)(void *));

void waitForChildFails(unsigned int waitFlag, int cloneFlags, int (*childRunFunction)(void *),
                       int (*parentRunFunction)(void *));

std::string get_tmp_path();

struct MemoryMapping {
  uintptr_t start;
  uintptr_t end;
  std::string perms;
  size_t offset;
  std::string device;
  size_t inode;
  std::string pathname;
};

std::optional<MemoryMapping> find_memory_mapping(uintptr_t addr, std::string_view maps);

#endif  // SRC_STARNIX_TESTS_SYSCALLS_TEST_HELPER_H_
