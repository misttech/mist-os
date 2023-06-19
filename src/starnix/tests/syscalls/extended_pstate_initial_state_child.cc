// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstddef>
#include <cstdint>

// This test examines the initial extended processor state when entering a new process.
// This means it must start at _start and not depend on any of the usual initialization logic
// performed by libc that may clobber this state before we have a chance to examine it. Thus
// it has some assembly helpers to issue syscalls standalone.

void exit_success() {
#if defined(__x86_64__)
  asm volatile(
      "mov $0, %rdi\n"
      "mov $231, %rax\n"
      "syscall\n");
#elif defined(__aarch64__)
  asm volatile(
      "mov x8, 94\n"
      "mov x0, 0\n"
      "svc #0\n");
#else
  // TODO(fxbug.dev/128554): Implement RISC-V support.
#error "unimplemented"
#endif
}

void exit_failure(int a) {
#if defined(__x86_64__)
  asm volatile(
      "movl %0, %%edi\n"
      "syscall\n"
      :
      : "r"(a), "a"(231)
      :);
#elif defined(__aarch64__)
  asm volatile(
      "mov w0, %w0\n"
      "mov x8, 94\n"
      "svc #0\n"
      :
      : "r"(a)
      :);
#else
  // TODO(fxbug.dev/128554): Implement RISC-V support.
#error "unimplemented"
#endif
}

void write_stderr(const char* str, size_t len) {
#if defined(__x86_64__)
  asm volatile("syscall" : : "D"(2), "S"(str), "d"(len), "a"(1) :);
#elif defined(__aarch64__)
  asm volatile(
      "mov x0, 2\n"
      "mov x1, %0\n"
      "mov x2, %1\n"
      "mov x8, 64\n"
      "svc #0\n"
      :
      : "r"(str), "r"(len)
      :);
#else
  // TODO(fxbug.dev/128554): Implement RISC-V support.
#error "unimplemented"
#endif
}

extern "C" void _start() {
#if defined(__x86_64__)
  constexpr size_t kXmmRegisterWidth = 128;
  std::byte buffer[kXmmRegisterWidth * 16];

#define MOVUPS_XMM_TO_ADDR(reg, addr) asm("movups %%xmm" #reg ", %0" : : "m"(addr) : "memory");
  MOVUPS_XMM_TO_ADDR(0, buffer[0 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(1, buffer[1 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(2, buffer[2 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(3, buffer[3 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(4, buffer[4 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(5, buffer[5 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(6, buffer[6 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(7, buffer[7 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(8, buffer[8 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(9, buffer[9 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(10, buffer[10 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(11, buffer[11 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(12, buffer[12 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(13, buffer[13 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(14, buffer[14 * kXmmRegisterWidth]);
  MOVUPS_XMM_TO_ADDR(15, buffer[15 * kXmmRegisterWidth]);
#undef MOVUPS_XMM_TO_ADDR

  for (size_t i = 0; i < kXmmRegisterWidth * 16; ++i) {
    if (buffer[i] != std::byte{0}) {
      int register_number = static_cast<int>(i) / kXmmRegisterWidth;
      char message[] = {'X', 'M', 'M', '0', '0', '\n'};
      message[4] = '0' + register_number % 10;
      message[3] = '0' + (register_number / 10) % 10;
      write_stderr(message, 6);
      exit_failure(static_cast<int>(i / kXmmRegisterWidth) + 1);
    }
  }
#elif defined(__aarch64__)
  constexpr size_t kQRegisterWidth = 128;
  std::byte q_register_buffer[32 * kQRegisterWidth];
  uint64_t fpcr = 0;
  uint64_t fpsr = 0;
  __asm__ volatile(
      "stp      q0,  q1, [%2, #(0 * 32)]\n"
      "stp      q2,  q3, [%2, #(1 * 32)]\n"
      "stp      q4,  q5, [%2, #(2 * 32)]\n"
      "stp      q6,  q7, [%2, #(3 * 32)]\n"
      "stp      q8,  q9, [%2, #(4 * 32)]\n"
      "stp     q10, q11, [%2, #(5 * 32)]\n"
      "stp     q12, q13, [%2, #(6 * 32)]\n"
      "stp     q14, q15, [%2, #(7 * 32)]\n"
      "stp     q16, q17, [%2, #(8 * 32)]\n"
      "stp     q18, q19, [%2, #(9 * 32)]\n"
      "stp     q20, q21, [%2, #(10 * 32)]\n"
      "stp     q22, q23, [%2, #(11 * 32)]\n"
      "stp     q24, q25, [%2, #(12 * 32)]\n"
      "stp     q26, q27, [%2, #(13 * 32)]\n"
      "stp     q28, q29, [%2, #(14 * 32)]\n"
      "stp     q30, q31, [%2, #(15 * 32)]\n"
      "mrs     %0, fpcr\n"
      "mrs     %1, fpsr\n"
      : "=r"(fpcr), "=r"(fpsr)
      : "r"(q_register_buffer)
      : "memory");
  for (size_t i = 0; i < 32 * kQRegisterWidth; ++i) {
    if (q_register_buffer[i] != std::byte{0}) {
      int register_number = static_cast<int>(i / kQRegisterWidth);
      char message[] = {'Q', '0', '0', '\n'};
      message[2] = '0' + register_number % 10;
      message[1] = '0' + (register_number / 10) % 10;
      write_stderr(message, 4);
      exit_failure(register_number + 1);
    }
  }
  if (fpcr != 0) {
    write_stderr("fpcr\n", 5);
    exit_failure(33);
  }
  if (fpsr != 0) {
    write_stderr("fpsr\n", 5);
    exit_failure(34);
  }
#else
  // TODO(fxbug.dev/128554): Implement RISC-V support.
#error "unimplemented"
#endif
  exit_success();
}
