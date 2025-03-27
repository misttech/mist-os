// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// AArch32 doesn't have extneded pstate
#if !defined(__arm__)

#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

#if defined(__riscv)
// Only VLEN=128 is supported currently.
const size_t RISCV_VLEN = 128;

// The following constants and structs duplicate the definitions that are
// available only with newer UAPI headers.
// TODO(b/325538967): Remove once we the headers used to compile this test are updated.
#define RISCV_V_MAGIC 0x53465457U
#define END_MAGIC 0x0U
#define END_HDR_SIZE 0x0U

struct riscv_ctx_hdr {
  uint32_t magic;
  uint32_t size;
};

struct riscv_v_ext_state {
  uint64_t vstart;
  uint64_t vl;
  uint64_t vtype;
  uint64_t vcsr;
  uint64_t vlenb;
  void* datap;
};
#endif  // defined(__riscv)

struct RegistersValue {
  // The tests below set and then check 32 bytes in FP or SIMD registers. Which registers are
  // being used is architecture-specific.
  uint64_t x[4];

  bool operator==(const RegistersValue& other) const {
    return x[0] == other.x[0] && x[1] == other.x[1] && x[2] == other.x[2] && x[3] == other.x[3];
  }
};

// Stores a 128-bit value in SIMD registers used for the test.
void SetTestRegisters(const RegistersValue* value) {
#if defined(__x86_64__)
  asm volatile(
      "movups  0(%0), %%xmm0\n"
      "movups 16(%0), %%xmm1\n"
      :
      : "r"(value));
#elif defined(__aarch64__)
  asm volatile(
      "ldr q0, [%0, #0]\n"
      "ldr q1, [%0, #16]\n"
      :
      : "r"(value));
#elif defined(__riscv)
  // Save two D registers and one V register.
  asm volatile(
      "fld f0, 0(%0)\n"
      "fld f1, 8(%0)\n"
      "li      a1, 16\n"
      "vsetvli zero, a1, e8, m8, ta, ma\n"
      "addi    %0, %0, 16\n"
      "vle8.v  v0, (%0)\n"
      :
      : "r"(value)
      : "a1");
#else
#error Add support for this architecture
#endif
}

// Reads a 128-bit value from the SIMD registers that were set in SetTestRegisters().
RegistersValue GetTestRegisters() {
  RegistersValue value;

#if defined(__x86_64__)
  asm volatile(
      "movups %%xmm0,  0(%0)\n"
      "movups %%xmm1, 16(%0)\n"
      :
      : "r"(&value));
#elif defined(__aarch64__)
  asm volatile(
      "str q0, [%0, #0]\n"
      "str q1, [%0, #16]\n"
      :
      : "r"(&value));
#elif defined(__riscv)
  asm volatile(
      "fsd     f0, 0(%0)\n"
      "fsd     f1, 8(%0)\n"
      "li      a1, 16\n"
      "vsetvli zero, a1, e8, m8, ta, ma\n"
      "addi    %0, %0, 16\n"
      "vse8.v  v0, 0(%0)\n"
      :
      : "r"(&value)
      : "a1");
#else
#error Add support for this architecture
#endif

  return value;
}

RegistersValue GetTestRegistersFromUcontext(ucontext_t* ucontext) {
  RegistersValue result;

#if defined(__x86_64__)
  auto fpregs = ucontext->uc_mcontext.fpregs;
  memcpy(&result, reinterpret_cast<void*>(fpregs->_xmm), sizeof(result));
  auto fpstate_ptr = reinterpret_cast<char*>(fpregs);

  // Bytes 464..512 in the XSAVE area are not used by XSAVE. Linux uses these bytes to store
  // `struct _fpx_sw_bytes`, which declares the set of extensions that may follow immediately
  // after `fpstate`. The region is marked with two "magic" values. Check that they are set
  // correctly.
  auto sw_bytes = reinterpret_cast<_fpx_sw_bytes*>(fpstate_ptr + 464);
  EXPECT_EQ(sw_bytes->magic1, FP_XSTATE_MAGIC1);
  uint32_t* magic2_ptr =
      reinterpret_cast<uint32_t*>(fpstate_ptr + sw_bytes->extended_size - FP_XSTATE_MAGIC2_SIZE);
  EXPECT_EQ(*magic2_ptr, FP_XSTATE_MAGIC2);
#elif defined(__aarch64__)
  fpsimd_context* fp_context = reinterpret_cast<fpsimd_context*>(ucontext->uc_mcontext.__reserved);
  EXPECT_EQ(fp_context->head.magic, static_cast<uint32_t>(FPSIMD_MAGIC));
  EXPECT_EQ(fp_context->head.size, sizeof(fpsimd_context));
  memcpy(&result, fp_context->vregs, sizeof(result));
#elif defined(__riscv)
  // Copy first 2 values from the F registers
  memcpy(&result, reinterpret_cast<void*>(ucontext->uc_mcontext.__fpregs.__d.__f),
         sizeof(uint64_t) * 2);

  // The header for the first RISC-V context extension is at the end of `uc_mcontext`.
  riscv_ctx_hdr* hdr = reinterpret_cast<riscv_ctx_hdr*>(&(ucontext->uc_mcontext) + 1) - 1;

  EXPECT_EQ(hdr->magic, RISCV_V_MAGIC);
  // Assuming VLEN=128 we meed 512 bytes to store 32 V registers.
  EXPECT_EQ(hdr->size, sizeof(riscv_ctx_hdr) + sizeof(riscv_v_ext_state) + 512);

  riscv_v_ext_state* v_state = reinterpret_cast<riscv_v_ext_state*>(hdr + 1);
  EXPECT_EQ(v_state->vlenb, RISCV_VLEN / 8);
  EXPECT_EQ(hdr->size, sizeof(riscv_ctx_hdr) + sizeof(riscv_v_ext_state) + RISCV_VLEN / 8 * 32);
  EXPECT_EQ(v_state->datap, v_state + 1);

  // Copy the last 2 values from the V registers.
  memcpy(&result.x[2], reinterpret_cast<void*>(v_state->datap), sizeof(uint64_t) * 2);

  riscv_ctx_hdr* end_hdr =
      reinterpret_cast<riscv_ctx_hdr*>(reinterpret_cast<uint8_t*>(hdr) + hdr->size);
  EXPECT_EQ(end_hdr->size, END_MAGIC);
  EXPECT_EQ(end_hdr->size, END_HDR_SIZE);
#else
#error Add support for this architecture
#endif

  return result;
}

// FP/SIMD registers should be initialized to 0 for new processes.
TEST(ExtendedPstate, InitialState) {
  // When running in Starnix the child binary is mounted at this path in the test's namespace.
  std::string child_path = "data/tests/extended_pstate_initial_state_child";
  if (!files::IsFile(child_path)) {
    // When running on host the child binary is next to the test binary.
    char self_path[PATH_MAX];
    realpath("/proc/self/exe", self_path);

    child_path =
        files::JoinPath(files::GetDirectoryName(self_path), "extended_pstate_initial_state_child");
  }
  ASSERT_TRUE(files::IsFile(child_path)) << child_path;
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&child_path] {
    // Set some registers. execve() should reset them to 0.
    const auto kTestData = RegistersValue{
        {0x0102030405060708, 0x090a0b0c0d0e0f10, 0x1112131415161718, 0x191a1b1c1d1e1f20}};
    SetTestRegisters(&kTestData);

    char* argv[] = {nullptr};
    char* envp[] = {nullptr};
    ASSERT_EQ(execve(child_path.c_str(), argv, envp), 0)
        << "execve error: " << errno << " (" << strerror(errno) << ")";
  });
}

// Verify that FP/SIMD registers are preserved by syscalls.
TEST(ExtendedPstate, Syscall) {
  const auto kTestRegisters = RegistersValue{
      {0x0102030405060708, 0x090a0b0c0d0e0f10, 0x1112131415161718, 0x191a1b1c1d1e1f20}};

  SetTestRegisters(&kTestRegisters);

  // Make several syscalls. Kernel uses floating point to generate `/proc/uptime` content, which
  // may affect the registers being tested.
  int fd = open("/proc/uptime", O_RDONLY);
  EXPECT_GT(fd, 0);
  char c;
  EXPECT_EQ(read(fd, &c, 1), 1);
  EXPECT_EQ(close(fd), 0);

  EXPECT_EQ(GetTestRegisters(), kTestRegisters);
}

struct SignalHandlerData {
  void* sigsegv_target;
  bool received_sigsegv = false;
  RegistersValue sigsegv_regs;

  RegistersValue sigusr1_regs;
};

SignalHandlerData signal_data;

// FP/SIMD registers are expected to be restored when returning form signal handlers
TEST(ExtendedPstate, Signals) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
    void* target = mmap(nullptr, page_size, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    ASSERT_NE(target, MAP_FAILED);
    signal_data.sigsegv_target = target;

    // Register SIGUSR1 handler.
    struct sigaction sigusr1_action = {};
    sigusr1_action.sa_sigaction = [](int sig, siginfo_t* info, void* ucontext) {
      if (sig != SIGUSR1) {
        _exit(1);
      }
      signal_data.sigusr1_regs =
          GetTestRegistersFromUcontext(reinterpret_cast<ucontext_t*>(ucontext));

      // Reset registers content.
      RegistersValue zero{{0, 0}};
      SetTestRegisters(&zero);
    };
    sigusr1_action.sa_flags = SA_SIGINFO;
    SAFE_SYSCALL(sigaction(SIGUSR1, &sigusr1_action, nullptr));

    // Register SIGSEGV handler.
    struct sigaction sigserv_action = {};
    signal_data.received_sigsegv = false;
    sigserv_action.sa_sigaction = [](int sig, siginfo_t* info, void* ucontext) {
      if (sig != SIGSEGV || info->si_addr != signal_data.sigsegv_target) {
        _exit(1);
      }
      signal_data.received_sigsegv = true;
      signal_data.sigsegv_regs =
          GetTestRegistersFromUcontext(reinterpret_cast<ucontext_t*>(ucontext));

      // Set registers to a value different from what it was outside of the signal handler.
      const auto kNestedRegs = RegistersValue{
          {0x191a1b1c1d1e1f20, 0x1112131415161718, 0x090a0b0c0d0e0f10, 0x0102030405060708}};
      SetTestRegisters(&kNestedRegs);

      // Raise another signal.
      raise(SIGUSR1);

      // Nested signal handler should preserve all registers.
      EXPECT_EQ(GetTestRegisters(), kNestedRegs);

      // Nested signal handler should receive values at the time it was invoked.
      EXPECT_EQ(signal_data.sigusr1_regs, kNestedRegs);

      // TODO: mprotect is not listed in signal-safety(7), should issue raw syscall
      mprotect(info->si_addr, 4096, PROT_READ | PROT_WRITE);
    };
    sigserv_action.sa_flags = SA_SIGINFO;
    SAFE_SYSCALL(sigaction(SIGSEGV, &sigserv_action, nullptr));

    const auto kTestRegsValue = RegistersValue{
        {0x0102030405060708, 0x090a0b0c0d0e0f10, 0x1112131415161718, 0x191a1b1c1d1e1f20}};
    SetTestRegisters(&kTestRegsValue);

    // Issue a store that will generate fault which will be fixed in SIGSEGV handler.
    asm volatile("" ::: "memory");
    *static_cast<char*>(target) = 1;
    asm volatile("" ::: "memory");
    ASSERT_TRUE(signal_data.received_sigsegv);

    // Check that the SIMD registers were preserved.
    EXPECT_EQ(GetTestRegisters(), kTestRegsValue);

    // Validate the registers value passed to the SIGSEGV handler in ucontext.
    EXPECT_EQ(signal_data.sigsegv_regs, kTestRegsValue);
  });

  ASSERT_TRUE(helper.WaitForChildren());
}

}  // namespace

#endif  // defined(__arm__)
