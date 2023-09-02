// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include <atomic>
#include <new>
#include <vector>

#include <fbl/string_printf.h>
#include <linux/futex.h>
#include <perftest/perftest.h>

namespace {

void WaitForChild(pid_t pid) {
  int status;
  pid_t child_pid = waitpid(pid, &status, 0);
  FX_CHECK(child_pid == pid);
  FX_CHECK(WIFEXITED(status));
  FX_CHECK(WEXITSTATUS(status) == 0);
}

// ScopedMapping returns an mmapped region of memory that gets unmapped when it
// goes out of scope.
class ScopedMapping {
 public:
  ScopedMapping(size_t size, int flags) : size_(size) {
    void* addr = mmap(NULL, size_, PROT_READ | PROT_WRITE, flags, -1, 0);
    FX_CHECK(addr != MAP_FAILED);
    base_ = reinterpret_cast<uintptr_t>(addr);
  }

  ~ScopedMapping() { reset(); }

  ScopedMapping(const ScopedMapping&) = delete;
  ScopedMapping& operator=(const ScopedMapping&) = delete;

  uintptr_t base() const { return base_; }

  size_t size() const { return size_; }

 private:
  void reset() {
    if (base_ != 0 && size_ != 0) {
      FX_CHECK(munmap(reinterpret_cast<void*>(base_), size_) == 0);
      base_ = 0;
      size_ = 0;
    }
  }

  uintptr_t base_;
  size_t size_;
};

// A wrapper for the futex system call, that only supports FUTEX_WAIT and
// FUTEX_WAKE, without timeout.
long SimpleFutex(std::atomic<uint32_t>* uaddr, int futex_op, uint32_t val) {
  return syscall(SYS_futex, reinterpret_cast<uint32_t*>(uaddr), futex_op, val, NULL, NULL, 0);
}

// A class similar to std::latch that works across processes.
// std::latch is not guaranteed to work across processes.
class Latch {
 public:
  Latch(size_t expected) : pending_(expected), done_(0) { FX_CHECK(expected > 0); }

  // Decrement the counter atomically, in a non-blocking manner.
  void CountDown() {
    if (pending_.fetch_sub(1) == 1) {
      done_ = 1;
      long res = SimpleFutex(&done_, FUTEX_WAKE, INT_MAX);
      FX_CHECK(res >= 0);
    }
  }

  // Blocks until the counter reaches 0.
  void Wait() {
    while (done_ == 0) {
      long res = SimpleFutex(&done_, FUTEX_WAIT, 0);
      FX_CHECK(res == 0 || (res == -1 && errno == EAGAIN));
    }
    FX_CHECK(pending_ == 0);
  }

  Latch(const Latch&) = delete;
  Latch& operator=(const Latch&) = delete;

 private:
  std::atomic<size_t> pending_;
  std::atomic<uint32_t> done_;
};

class Sync {
 public:
  // main process will wait on pending_children
  Latch pending_children;

  // children will wait on parent_done before exiting.
  Latch parent_done;

  Sync(size_t expected_children) : pending_children(expected_children), parent_done(1) {}
};

// Measures the time taken by calling fork once.
//
// The fork part of the measurement will include the time required for the
// child to start running and writing to a page shared with the parent
// process. The wait part of the measurement includes all the process teardown
// and signaling until the child process finishes.
bool Fork(perftest::RepeatState* state) {
  // Use a shared mapping to synchronize with the parent process.
  ScopedMapping mapping(sizeof(Sync), MAP_SHARED | MAP_ANONYMOUS | MAP_POPULATE);
  state->DeclareStep("fork");
  state->DeclareStep("wait");

  while (state->KeepRunning()) {
    Sync* sync = new (reinterpret_cast<void*>(mapping.base())) Sync(1);
    pid_t pid = fork();
    FX_CHECK(pid >= 0);

    if (pid == 0) {
      sync->pending_children.CountDown();
      // Wait until the next step before exiting.
      sync->parent_done.Wait();
      _exit(EXIT_SUCCESS);
    }
    sync->pending_children.Wait();

    // At this point, all children have started executing.
    // Stop measuring fork time and start measuring wait time.
    state->NextStep();
    sync->parent_done.CountDown();

    WaitForChild(pid);
    sync->~Sync();
  }
  return true;
}

// Measures the time taken by calling fork n times.
//
// The fork part of the measurement will include the time required for all the
// children to start running and writing to a page shared with the parent
// process. The wait part of the measurement includes all the process teardown
// and signaling until all the child processes have finished.
bool ForkMultiple(perftest::RepeatState* state, size_t n) {
  std::vector<pid_t> pids(n, -1);

  // Use a shared mapping to synchronize with the parent process.
  ScopedMapping mapping(sizeof(Sync), MAP_SHARED | MAP_ANONYMOUS | MAP_POPULATE);

  state->DeclareStep("fork");
  state->DeclareStep("wait");

  while (state->KeepRunning()) {
    Sync* sync = new (reinterpret_cast<void*>(mapping.base())) Sync(n);

    // Parent process will fork n times.
    for (size_t i = 0; i < n; i++) {
      pid_t pid = fork();
      FX_CHECK(pid >= 0);

      if (pid == 0) {
        sync->pending_children.CountDown();

        // Wait until the next step before exiting.
        sync->parent_done.Wait();
        _exit(EXIT_SUCCESS);
      }
      pids[i] = pid;
    }
    sync->pending_children.Wait();

    // At this point, all children have started executing.
    // Stop measuring fork time and start measuring wait time.
    state->NextStep();
    sync->parent_done.CountDown();

    for (pid_t pid : pids) {
      WaitForChild(pid);
    }
    sync->~Sync();
  }

  return true;
}

pid_t ForkRecursive(Sync* sync, size_t n) {
  FX_CHECK(n > 0);

  pid_t pid = fork();
  FX_CHECK(pid >= 0);
  if (pid == 0) {
    // Child process.
    // Fork recursively and wait for that child to finish.
    sync->pending_children.CountDown();
    if (n == 1) {
      // Last child, nothing to be done.
      sync->parent_done.Wait();
    } else {
      pid_t pid = ForkRecursive(sync, n - 1);
      sync->parent_done.Wait();
      WaitForChild(pid);
    }
    _exit(EXIT_SUCCESS);
  }

  return pid;
}

// Measures the time taken by creating a chain of n nested forks.
//
// The fork part of the measurement will include the time required for all the
// children to start running and writing to a page shared with the parent
// process. The wait part of the measurement includes all the process teardown
// and signaling until all the child processes have finished.
bool ForkMultipleNested(perftest::RepeatState* state, size_t n) {
  // Use a shared mapping to synchronize with the parent process.
  ScopedMapping mapping(sizeof(Sync), MAP_SHARED | MAP_ANONYMOUS | MAP_POPULATE);

  state->DeclareStep("fork");
  state->DeclareStep("wait");

  while (state->KeepRunning()) {
    Sync* sync = new (reinterpret_cast<void*>(mapping.base())) Sync(n);

    pid_t pid = ForkRecursive(sync, n);

    sync->pending_children.Wait();
    // At this point, all children have started executing.
    // Stop measuring fork time and start measuring wait time.
    state->NextStep();
    sync->parent_done.CountDown();

    WaitForChild(pid);

    sync->~Sync();
  }
  return true;
}

void RegisterTests() {
  perftest::RegisterTest("Fork", Fork);

  for (size_t length : {8, 64}) {
    auto test_name = fbl::StringPrintf("ForkMultiple/%zu", length);
    perftest::RegisterTest(test_name.c_str(), ForkMultiple, length);
  }

  for (size_t length : {8, 64}) {
    auto test_name = fbl::StringPrintf("ForkMultipleNested/%zu", length);
    perftest::RegisterTest(test_name.c_str(), ForkMultipleNested, length);
  }
}
PERFTEST_CTOR(RegisterTests)

}  // namespace
