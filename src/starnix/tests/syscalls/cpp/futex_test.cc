// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <atomic>
#include <ctime>
#include <latch>
#include <new>
#include <string>
#include <thread>
#include <vector>

#include <gtest/gtest.h>
#include <linux/futex.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

struct robust_list_entry {
  struct robust_list *next;
  int futex;
};

// Tests that robust_lists set futex FUTEX_OWNER_DIED bit if the thread that locked a futex
// dies without unlocking it.
TEST(RobustFutexTest, FutexStateCheck) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    robust_list_entry entry = {.next = nullptr, .futex = 0};
    robust_list_head head = {.list = {.next = nullptr},
                             .futex_offset = offsetof(robust_list_entry, futex),
                             .list_op_pending = nullptr};

    std::thread t([&entry, &head]() {
      head.list.next = reinterpret_cast<struct robust_list *>(&entry);
      EXPECT_EQ(0, syscall(SYS_set_robust_list, &head, sizeof(robust_list_head)));
      entry.futex = static_cast<int>(syscall(SYS_gettid));
      entry.next = reinterpret_cast<struct robust_list *>(&head);
      // Thread dies without releasing futex, so futex's FUTEX_OWNER_DIED bit is set.
    });
    t.join();
    EXPECT_EQ(FUTEX_OWNER_DIED, entry.futex & FUTEX_OWNER_DIED);
  });
}

// Tests that entries with a tid different than the current tid are ignored.
TEST(RobustFutexTest, OtherTidsAreIgnored) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    constexpr size_t kNumEntries = 3;
    robust_list_entry entries[kNumEntries] = {};
    robust_list_head head = {.list = {.next = nullptr},
                             .futex_offset = offsetof(robust_list_entry, futex),
                             .list_op_pending = nullptr};

    head.list.next = reinterpret_cast<struct robust_list *>(&entries[0]);
    for (size_t i = 0; i < kNumEntries - 1; i++) {
      entries[i].next = reinterpret_cast<struct robust_list *>(&entries[i + 1]);
    }
    entries[kNumEntries - 1].next = reinterpret_cast<struct robust_list *>(&head);

    int parent_tid = static_cast<int>(syscall(SYS_gettid));

    std::thread t([&entries, &head, parent_tid]() {
      EXPECT_EQ(0, syscall(SYS_set_robust_list, &head, sizeof(robust_list_head)));
      int tid = static_cast<int>(syscall(SYS_gettid));
      entries[0].futex = tid;
      entries[1].futex = parent_tid;
      entries[2].futex = tid;
    });
    t.join();
    // We expect the first and list entries to be correctly modified.
    // The second entry (wrong tid) should remain unchanged.
    EXPECT_EQ(FUTEX_OWNER_DIED, entries[0].futex & FUTEX_OWNER_DIED);
    EXPECT_EQ(FUTEX_OWNER_DIED, entries[2].futex & FUTEX_OWNER_DIED);
    EXPECT_EQ(parent_tid, entries[1].futex);
  });
}

// Tests that an entry with next = NULL doesn't cause issues.
TEST(RobustFutexTest, NullEntryStopsProcessing) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    robust_list_entry entry = {.next = nullptr, .futex = 0};
    robust_list_head head = {.list = {.next = nullptr},
                             .futex_offset = offsetof(robust_list_entry, futex),
                             .list_op_pending = nullptr};

    std::thread t([&entry, &head]() {
      head.list.next = reinterpret_cast<struct robust_list *>(&entry);
      EXPECT_EQ(0, syscall(SYS_set_robust_list, &head, sizeof(robust_list_head)));
      entry.futex = static_cast<int>(syscall(SYS_gettid));
      entry.next = nullptr;
    });
    t.join();
    // We expect the first entry to be correctly modified.
    EXPECT_EQ(FUTEX_OWNER_DIED, entry.futex & FUTEX_OWNER_DIED);
  });
}

// Test that exceeding the maximum number of robust futexes would lead to a
// futex not being processed.
TEST(RobustFutexTest, RobustListLimitIsEnforced) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    constexpr size_t kNumEntries = ROBUST_LIST_LIMIT + 1;
    robust_list_entry entries[kNumEntries] = {};
    robust_list_head head = {.list = {.next = nullptr},
                             .futex_offset = offsetof(robust_list_entry, futex),
                             .list_op_pending = nullptr};

    head.list.next = reinterpret_cast<struct robust_list *>(&entries[0]);
    for (size_t i = 0; i < kNumEntries - 1; i++) {
      entries[i].next = reinterpret_cast<struct robust_list *>(&entries[i + 1].next);
    }
    entries[kNumEntries - 1].next = reinterpret_cast<struct robust_list *>(&head);

    std::thread t([&entries, &head]() {
      int tid = static_cast<int>(syscall(SYS_gettid));
      for (size_t i = 0; i < kNumEntries; i++) {
        entries[i].futex = tid;
      }

      EXPECT_EQ(0, syscall(SYS_set_robust_list, &head, sizeof(robust_list_head)));
    });

    t.join();

    for (size_t i = 0; i < kNumEntries - 1; i++) {
      EXPECT_EQ(FUTEX_OWNER_DIED, entries[i].futex & FUTEX_OWNER_DIED);
    }
    // The last entry was not modified.
    EXPECT_EQ(0, entries[kNumEntries - 1].futex & FUTEX_OWNER_DIED);
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

struct unaligned_robust_list_entry {
  struct robust_list *next;
  char unused;
  int futex;
} __attribute__((packed, aligned(4)));
static_assert(offsetof(unaligned_robust_list_entry, futex) % 4 != 0,
              "futex lock offset must be unaligned");

TEST(RobustFutexTest, RobustListEnforcesAlignment) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    unaligned_robust_list_entry entry = {.next = nullptr, .unused = 0, .futex = 0};
    robust_list_head head = {.list = {.next = nullptr},
                             .futex_offset = offsetof(unaligned_robust_list_entry, futex),
                             .list_op_pending = nullptr};

    std::thread t([&entry, &head]() {
      head.list.next = reinterpret_cast<struct robust_list *>(&entry);
      EXPECT_EQ(0, syscall(SYS_set_robust_list, &head, sizeof(robust_list_head)));
      entry.futex = static_cast<int>(syscall(SYS_gettid));
      entry.next = reinterpret_cast<struct robust_list *>(&head);
    });
    t.join();
    // The entry was not modified.
    EXPECT_EQ(0, entry.futex & FUTEX_OWNER_DIED);
  });
}

TEST(RobustFutexTest, DoesNotModifyReadOnlyMapping) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    const size_t page_size = sysconf(_SC_PAGESIZE);
    void *addr =
        mmap(NULL, sizeof(page_size), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    ASSERT_NE(addr, MAP_FAILED);

    robust_list_head head = {.list = {.next = nullptr},
                             .futex_offset = offsetof(robust_list_entry, futex),
                             .list_op_pending = nullptr};

    robust_list_entry *entry = new (addr) robust_list_entry;

    std::thread t([entry, &head, addr, page_size]() {
      head.list.next = reinterpret_cast<struct robust_list *>(entry);
      EXPECT_EQ(0, syscall(SYS_set_robust_list, &head, sizeof(robust_list_head)));
      entry->futex = static_cast<int>(syscall(SYS_gettid));
      entry->next = reinterpret_cast<struct robust_list *>(&head);

      EXPECT_EQ(0, mprotect(addr, page_size, PROT_READ));
    });
    t.join();
    // Memory allocating futex is not writable, so it should not be modified by the kernel.
    EXPECT_EQ(0, entry->futex & FUTEX_OWNER_DIED);
    entry->~robust_list_entry();
    EXPECT_EQ(0, munmap(addr, page_size));
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

// Tests that issuing a cyclic robust list doesn't hang the starnix kernel.
TEST(RobustFutexTest, CyclicRobustListDoesntHang) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    robust_list_entry entry1 = {.next = nullptr, .futex = 0};
    robust_list_entry entry2 = {.next = nullptr, .futex = 0};
    robust_list_head head = {.list = {.next = nullptr},
                             .futex_offset = offsetof(robust_list_entry, futex),
                             .list_op_pending = nullptr};

    std::thread t([&entry1, &entry2, &head]() {
      entry1.next = reinterpret_cast<struct robust_list *>(&entry2);
      entry2.next = reinterpret_cast<struct robust_list *>(&entry1);

      head.list.next = reinterpret_cast<struct robust_list *>(&entry1);
      EXPECT_EQ(0, syscall(SYS_set_robust_list, &head, sizeof(robust_list_head)));
    });
    t.join();
    // Our robust list has a cycle. We should be able to stop correctly.
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

// Tests that robust_lists set futex FUTEX_OWNER_DIED bit if the thread that locked a futex
// executes an exec() without unlocking it.
TEST(RobustFutexTest, FutexStateAfterExecCheck) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    // Allocate the futex and the robust list in shared memory.
    void *shared = mmap(nullptr, sizeof(robust_list_entry) + sizeof(robust_list_head),
                        PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    EXPECT_NE(MAP_FAILED, shared) << "Error " << errno;

    robust_list_head *head = reinterpret_cast<robust_list_head *>(shared);
    robust_list_entry *entry = reinterpret_cast<robust_list_entry *>(
        reinterpret_cast<intptr_t>(shared) + sizeof(robust_list_head));

    *entry = {.next = reinterpret_cast<struct robust_list *>(head), .futex = 0};
    *head = {.list = {.next = reinterpret_cast<struct robust_list *>(entry)},
             .futex_offset = offsetof(robust_list_entry, futex),
             .list_op_pending = nullptr};

    // Create a pipe that the child can use to notify parent process when it is running.
    int pipefd[2];
    pipe(pipefd);

    // Create a file we can lock.  After it notifies us that it is running via
    // the pipe, the child will wait to terminate until we unlock the file.
    test_helper::ScopedTempFD terminate_child_fd;
    struct flock fl = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    EXPECT_EQ(0, fcntl(terminate_child_fd.fd(), F_SETLK, &fl));

    test_helper::ForkHelper helper;

    helper.RunInForkedProcess([&entry, &head, &terminate_child_fd, &pipefd] {
      // Redirect stdout to one end of the pipe
      EXPECT_NE(-1, dup2(pipefd[1], STDOUT_FILENO));
      EXPECT_EQ(0, syscall(SYS_set_robust_list, head, sizeof(robust_list_head)));
      entry->futex = static_cast<int>(syscall(SYS_gettid));

      std::string test_binary = "/data/tests/syscall_test_exec_child";
      if (!files::IsFile(test_binary)) {
        // We're running on host
        char self_path[PATH_MAX];
        realpath("/proc/self/exe", self_path);

        test_binary =
            files::JoinPath(files::GetDirectoryName(self_path), "syscall_test_exec_child");
      }
      char *const argv[] = {const_cast<char *>(test_binary.c_str()),
                            const_cast<char *>(terminate_child_fd.name().c_str()), nullptr};

      // execv happens without releasing futex, so futex's FUTEX_OWNER_DIED bit is set.
      execv(test_binary.c_str(), argv);
    });

    char buf[5];
    // Wait until the child process has performed the exec
    EXPECT_NE(-1, read(pipefd[0], reinterpret_cast<void *>(buf), 5));

    EXPECT_EQ(FUTEX_OWNER_DIED, entry->futex & FUTEX_OWNER_DIED);

    // Unlock the file, allowing the child process to continue (and exit).
    struct flock fl2 = {.l_type = F_UNLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    EXPECT_EQ(0, fcntl(terminate_child_fd.fd(), F_SETLK, &fl2));
    EXPECT_EQ(true, helper.WaitForChildren());
    munmap(shared, sizeof(robust_list_entry) + sizeof(robust_list_head));
  });
}

TEST(FutexTest, FutexAddressHasToBeAligned) {
  uint32_t some_addresses[] = {0, 0};
  uintptr_t addr = reinterpret_cast<uintptr_t>(&some_addresses[0]);

  auto futex_basic = [](uintptr_t addr, uint32_t op, uint32_t val) {
    return syscall(SYS_futex, addr, op, val, NULL, NULL, 0);
  };

  auto futex_requeue = [](uintptr_t addr, uint32_t val, uint32_t val2, uintptr_t addr2) {
    return syscall(SYS_futex, addr, FUTEX_REQUEUE, val, val2, addr2, 0);
  };

  for (size_t i = 1; i <= 3; i++) {
    EXPECT_EQ(-1, futex_basic(addr + i, FUTEX_WAIT, 0));
    EXPECT_EQ(errno, EINVAL);
    EXPECT_EQ(-1, futex_basic(addr + i, FUTEX_WAIT_PRIVATE, 0));
    EXPECT_EQ(errno, EINVAL);
    EXPECT_EQ(-1, futex_basic(addr + i, FUTEX_WAKE, 0));
    EXPECT_EQ(errno, EINVAL);
    EXPECT_EQ(-1, futex_basic(addr + i, FUTEX_WAKE_PRIVATE, 0));
    EXPECT_EQ(errno, EINVAL);
    EXPECT_EQ(-1, futex_requeue(addr, 0, 0, addr + 4 + i));
    EXPECT_EQ(errno, EINVAL);
  }
}

TEST(FutexTest, FutexAddressOutOfRange) {
  uintptr_t addr = static_cast<uintptr_t>(-4);  // not in userspace

  auto futex_basic = [](uintptr_t addr, uint32_t op, uint32_t val) {
    return syscall(SYS_futex, addr, op, val, NULL, NULL, 0);
  };

  EXPECT_EQ(-1, futex_basic(addr, FUTEX_WAIT, 0));
  EXPECT_EQ(errno, EFAULT);
  EXPECT_EQ(-1, futex_basic(addr, FUTEX_WAIT_PRIVATE, 0));
  EXPECT_EQ(errno, EFAULT);
}

TEST(FutexTest, FutexWaitOnRemappedMemory) {
  // This test is inherently racy, and could be flaky:
  // We are trying to race between the FUTEX_WAIT and mmap+FUTEX_WAKE
  // operations. We want to make sure that if we remap the futex
  // page, we don't get threads stuck.
  //
  // See b/298664027 for details.
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    constexpr size_t kNumWaiters = 16;
    constexpr uint32_t kFutexConstant = 0xbeef;
    const size_t page_size = sysconf(_SC_PAGESIZE);
    void *addr = mmap(NULL, page_size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    ASSERT_NE(addr, MAP_FAILED);
    auto futex_basic = [](std::atomic<uint32_t> *addr, uint32_t op, uint32_t val) {
      return syscall(SYS_futex, reinterpret_cast<uint32_t *>(addr), op, val, NULL, NULL, 0);
    };

    std::atomic<uint32_t> *futex = new (addr) std::atomic<uint32_t>(kFutexConstant);

    std::latch wait_for_all_threads(kNumWaiters + 1);

    std::vector<std::thread> waiters;
    for (size_t i = 0; i < kNumWaiters; i++) {
      waiters.emplace_back([&wait_for_all_threads, futex, &futex_basic]() {
        wait_for_all_threads.arrive_and_wait();

        while (futex->load() == kFutexConstant) {
          long res = futex_basic(futex, FUTEX_WAIT_PRIVATE, kFutexConstant);
          EXPECT_TRUE(res == 0 || (res == -1 && errno == EAGAIN));
        }
        EXPECT_EQ(futex->load(), 0u);
      });
    }

    wait_for_all_threads.arrive_and_wait();

    void *new_addr = mmap(addr, page_size, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    ASSERT_NE(new_addr, MAP_FAILED);
    ASSERT_EQ(new_addr, addr);
    futex->store(0);
    long res = futex_basic(futex, FUTEX_WAKE_PRIVATE, INT_MAX);
    EXPECT_TRUE(res >= 0);

    for (auto &waiter : waiters) {
      waiter.join();
    }
    EXPECT_EQ(0, munmap(addr, page_size));
  });
  EXPECT_EQ(true, helper.WaitForChildren());
}

// Test that FUTEX_WAIT can be restarted after being interrupted by a signal.
TEST(FutexTest, WaitRestartableOnSignal) {
  // The child process will do a FUTEX_WAIT with a timeout. The parent will send SIGSTOP + SIGCONT
  // during the timeout.
  test_helper::ForkHelper helper;
  pid_t child_pid = helper.RunInForkedProcess([] {
    uint32_t word = 0;
    struct timespec timeout = {.tv_sec = 1};
    // Should fail with ETIMEDOUT and *not* EINTR.
    ASSERT_THAT(syscall(SYS_futex, &word, FUTEX_WAIT_PRIVATE, 0, &timeout),
                SyscallFailsWithErrno(ETIMEDOUT));
  });

  // Wait for child to go to sleep.
  std::cerr << "wait for block" << std::endl;
  test_helper::WaitUntilBlocked(child_pid, true);
  std::cerr << "waited for block" << std::endl;
  usleep(100000);

  ASSERT_THAT(kill(child_pid, SIGSTOP), SyscallSucceeds());
  ASSERT_THAT(kill(child_pid, SIGCONT), SyscallSucceeds());
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST(FutexTest, CanRequeueAllWaiters) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    auto futex_basic = [](std::atomic<uint32_t> *addr, uint32_t op, uint32_t val) {
      return syscall(SYS_futex, addr, op, val, NULL, NULL, 0);
    };

    auto futex_requeue_all = [](std::atomic<uint32_t> *addr, std::atomic<uint32_t> *addr2) {
      return syscall(SYS_futex, addr, FUTEX_REQUEUE, 0, INT_MAX, addr2, 0);
    };

    std::atomic<uint32_t> futex_word = 0;
    std::atomic<uint32_t> requeue_futex_word = 0;
    std::atomic<size_t> awakened = 0;

    auto waiter_func = [&futex_word, &awakened, &futex_basic]() {
      long res = SAFE_SYSCALL(futex_basic(&futex_word, FUTEX_WAIT, 0));
      EXPECT_EQ(res, 0);
      awakened++;
    };

    std::vector<std::thread> threads;

    constexpr size_t kNumThreads = 10;
    for (size_t i = 0; i < kNumThreads; i++) {
      threads.push_back(std::thread(waiter_func));
    }

    EXPECT_EQ(awakened, 0u);

    long requeued = 0;
    while (requeued != kNumThreads) {
      requeued += SAFE_SYSCALL(futex_requeue_all(&futex_word, &requeue_futex_word));
      sched_yield();
    }

    EXPECT_EQ(awakened, 0u);

    // We cannot wake anyone in the first futex.
    futex_word = 1;
    EXPECT_EQ(futex_basic(&futex_word, FUTEX_WAKE, INT_MAX), 0);

    // We can wake kNumThreads in the second futex.
    requeue_futex_word = 1;
    while (awakened != kNumThreads) {
      SAFE_SYSCALL(futex_basic(&requeue_futex_word, FUTEX_WAKE, INT_MAX));
    }

    for (auto &thread : threads) {
      thread.join();
    }
  });
}

TEST(FutexTest, FutexFailsWithEFAULTOnNullAddress) {
  EXPECT_THAT(syscall(SYS_futex, nullptr, FUTEX_WAIT, 0, NULL, 0, 0),
              SyscallFailsWithErrno(EFAULT));
}

TEST(FutexTest, FutexFailsWithEFAULTOnInvalidLowAddress) {
  // Zircon forbids creating mappings with addresses lower than 2MB.
  constexpr uintptr_t kInvalidLowAddress = 0x10000;
  EXPECT_THAT(syscall(SYS_futex, kInvalidLowAddress, FUTEX_WAIT, 0, NULL, 0, 0),
              SyscallFailsWithErrno(EFAULT));
}

#if defined(__x86_64__)
// From zircon/kernel/arch/x86/include/arch/kernel_aspace.h
constexpr uintptr_t kLowestNormalModeAddress = ((1ULL << 46));
#elif defined(__aarch64__)
// From zircon/kernel/arch/arm64/include/arch/kernel_aspace.h
constexpr uintptr_t kLowestNormalModeAddress = ((1ULL << 47));
#elif defined(__riscv)
// From zircon/kernel/arch/riscv64/include/arch/kernel_aspace.h
// Currently we only support the RV39 address space model.
constexpr uintptr_t kLowestNormalModeAddress = ((1ULL << 37));
#else
#error Unsupported architecture
#endif

TEST(FutexTest, FutexFailsWithEFAULTOnLowestNormalAddress) {
  // The restricted / normal address space layout is Starnix-specific.
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP();
  }
  EXPECT_THAT(syscall(SYS_futex, kLowestNormalModeAddress, FUTEX_WAIT, 0, NULL, 0, 0),
              SyscallFailsWithErrno(EFAULT));
}

TEST(FutexTest, FutexSucceedsHighestRestrictedAddress) {
  // The restricted / normal address space layout is Starnix-specific.
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP();
  }
  const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGESIZE));
  const uintptr_t highest_restricted_mode_address = kLowestNormalModeAddress - page_size;
  void *result = mmap(reinterpret_cast<void *>(highest_restricted_mode_address), page_size,
                      PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
  ASSERT_NE(result, MAP_FAILED) << strerror(errno);
  ASSERT_EQ(highest_restricted_mode_address, reinterpret_cast<uintptr_t>(result));
  struct timespec wait_timeout = {};
  long futex_result =
      syscall(SYS_futex, highest_restricted_mode_address, FUTEX_WAIT, 0, &wait_timeout, 0, 0);
  EXPECT_EQ(futex_result, -1);
  EXPECT_EQ(errno, ETIMEDOUT);
  SAFE_SYSCALL(munmap(reinterpret_cast<void *>(highest_restricted_mode_address), page_size));
}

}  // anonymous namespace
