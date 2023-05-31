// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/test_helper.h"

void* volatile target;

TEST(Fault, PreserveVectorRegisters) {
  ForkHelper helper;
  helper.RunInForkedProcess([] {
    size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
    target = mmap(nullptr, page_size, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    ASSERT_NE(target, MAP_FAILED);
    // Register SIGSEGV handler
    struct sigaction act = {};
    act.sa_sigaction = [](int sig, siginfo_t* info, void* ucontext) {
      if (sig != SIGSEGV) {
        _exit(1);
      }
      // TODO: once we fill in siginfo_t we can instead do:
      // void* addr = info->si_addr;
      // TODO: mprotect is not listed in signal-safety(7), should issue raw syscall
      mprotect(target, 4096, PROT_READ | PROT_WRITE);
    };
    act.sa_flags = SA_SIGINFO;
    SAFE_SYSCALL(sigaction(SIGSEGV, &act, nullptr));
    // Store value to vector register
    std::vector buffer(16, 'a');
    char dest[16] = {};
    char* buffer_addr = buffer.data();
#if defined(__x86_64__)
    asm volatile(
        "movups (%0), %%xmm0\n"
        // Issue store that will generate fault which will be fixed in SIGSEGV handler
        "movb   $98, (%1)\n"
        "movups %%xmm0, %2\n"
        :
        : "r"(buffer_addr), "r"(target), "m"(dest)
        : "memory");
#elif defined(__aarch64__)
    asm volatile(
        "ldr q0, [%0]\n"
        "mov w8, #98\n"
        // Issue store that will generate fault which will be fixed in SIGSEGV handler
        "str w8, [%1]\n"
        "str q0, %2\n"
        :
        : "r"(buffer_addr), "r"(target), "m"(dest)
        : "memory", "w8");
#else
#error Add support for this architecture
#endif
    // Verify that value stored in vector register is still present
    for (size_t i = 0; i < 16; ++i) {
      EXPECT_EQ(buffer[i], 'a') << i;
      EXPECT_EQ(dest[i], 'a') << i;
    }
  });
  ASSERT_TRUE(helper.WaitForChildren());
}
